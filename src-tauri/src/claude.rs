use std::{io::ErrorKind, path::PathBuf, time::Duration};

use futures_util::StreamExt;
use serde_json::{json, Value};
use tokio::{fs, io::AsyncReadExt, process::Command};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::{config, logs::LogLevel, state::AppState};

const MAX_TURNS: usize = 20;
const MAX_TOKENS: u32 = 4096;
const MAX_FILE_CHARS: usize = 220_000;
const MAX_COMMAND_CHARS: usize = 120_000;
const CACHE_CONTROL_TTL: &str = "ephemeral";
const CACHE_PRIMER_WORDS: usize = 5_200;
const API_FIRST_BYTE_TIMEOUT_SECONDS: u64 = 45;
const API_STREAM_IDLE_TIMEOUT_SECONDS: u64 = 90;

#[derive(Debug, Clone)]
pub struct RuntimeConfig {
    pub messages_url: String,
    pub api_key: String,
    pub model: String,
}

#[derive(Debug, Default)]
struct ClaudeTurn {
    text: String,
    stop_reason: Option<String>,
    content: Vec<Value>,
    tool_uses: Vec<ToolUse>,
    usage: Option<Value>,
}

#[derive(Debug, Clone)]
struct ToolUse {
    id: String,
    name: String,
    input: Value,
}

#[derive(Debug, Default, Clone)]
struct ContentBlockBuilder {
    kind: String,
    id: Option<String>,
    name: Option<String>,
    text: String,
    input_json: String,
}

#[derive(Debug, Default)]
struct StreamEventStats {
    events: u64,
    text_chars: usize,
    tool_uses: u64,
    usage_events: u64,
}

pub fn load_runtime_config() -> anyhow::Result<RuntimeConfig> {
    let cfg = config::load_config();
    Ok(RuntimeConfig {
        messages_url: config::normalize_messages_url(&cfg.api_url)?,
        api_key: config::require_api_key()?,
        model: cfg.model,
    })
}

pub async fn test_connection(state: &AppState) -> anyhow::Result<String> {
    let runtime = load_runtime_config()?;
    let request_id = Uuid::new_v4().to_string();
    state.logs().push(
        LogLevel::Info,
        "new-api",
        Some(request_id.clone()),
        None,
        "开始测试 new-api 连接",
        Some(json!({"url": runtime.messages_url, "model": runtime.model, "api_key": runtime.api_key})),
    );

    let _upstream_guard = state.begin_upstream_request();
    let response = state
        .http()
        .post(&runtime.messages_url)
        .bearer_auth(&runtime.api_key)
        .header("anthropic-version", "2023-06-01")
        .json(&json!({
            "model": runtime.model,
            "max_tokens": 1,
            "messages": [{"role": "user", "content": "ping"}],
            "stream": false
        }))
        .send()
        .await?;

    let status = response.status();
    let body = response.text().await.unwrap_or_default();
    if status.is_success() {
        let payload = serde_json::from_str::<Value>(&body).unwrap_or_else(|_| json!({}));
        if let Some(usage) = payload.get("usage") {
            record_token_usage(state, usage, Some(request_id.clone()), None);
        }
        state.logs().push(
            LogLevel::Info,
            "new-api",
            Some(request_id),
            None,
            "new-api 连接成功",
            Some(json!({"status": status.as_u16(), "usage": payload.get("usage")})),
        );
        Ok("连接成功".to_string())
    } else {
        state.logs().push(
            LogLevel::Error,
            "new-api",
            Some(request_id),
            None,
            "new-api 连接失败",
            Some(json!({"status": status.as_u16(), "body": truncate(&body, 2000)})),
        );
        anyhow::bail!("new-api 返回 {}：{}", status.as_u16(), truncate(&body, 400));
    }
}

pub async fn run_agent(
    state: AppState,
    prompt: String,
    cwd: PathBuf,
    task_id: String,
    cancel: CancellationToken,
) -> anyhow::Result<String> {
    let runtime = load_runtime_config()?;
    let mut messages = vec![json!({
        "role": "user",
        "content": prompt
    })];
    let mut final_text = String::new();

    for turn_index in 0..MAX_TURNS {
        if cancel.is_cancelled() {
            anyhow::bail!("任务已取消");
        }

        state.logs().push(
            LogLevel::Info,
            "agent",
            None,
            Some(task_id.clone()),
            format!("开始第 {} 轮 Claude 调用", turn_index + 1),
            Some(json!({"workdir": cwd, "model": runtime.model})),
        );

        let turn = stream_turn(&state, &runtime, &messages, &task_id, &cancel).await?;
        final_text.push_str(&turn.text);
        messages.push(json!({
            "role": "assistant",
            "content": turn.content
        }));

        if turn.tool_uses.is_empty() {
            state.logs().push(
                LogLevel::Info,
                "agent",
                None,
                Some(task_id.clone()),
                "Claude 返回最终结果",
                Some(json!({"stop_reason": turn.stop_reason})),
            );
            return Ok(final_text.trim().to_string());
        }

        let mut tool_results = Vec::new();
        for tool_use in turn.tool_uses {
            if cancel.is_cancelled() {
                anyhow::bail!("任务已取消");
            }
            let result = execute_local_tool(&state, &cwd, &task_id, &tool_use, &cancel).await;
            let (content, is_error) = match result {
                Ok(value) => (value, false),
                Err(error) => (error.to_string(), true),
            };
            tool_results.push(tool_result_block(&tool_use.id, content, is_error));
        }
        mark_last_tool_result_for_cache(&mut tool_results);
        messages.push(json!({
            "role": "user",
            "content": tool_results
        }));
    }

    anyhow::bail!("Claude 工具循环超过 {} 轮，已停止", MAX_TURNS)
}

async fn stream_turn(
    state: &AppState,
    runtime: &RuntimeConfig,
    messages: &[Value],
    task_id: &str,
    cancel: &CancellationToken,
) -> anyhow::Result<ClaudeTurn> {
    let request_id = Uuid::new_v4().to_string();
    let started = std::time::Instant::now();
    state.logs().push(
        LogLevel::Info,
        "new-api",
        Some(request_id.clone()),
        Some(task_id.to_string()),
        "发送 Claude Messages 流式请求",
        Some(json!({
            "url": runtime.messages_url,
            "model": runtime.model,
            "api_key": runtime.api_key,
            "message_count": messages.len()
        })),
    );

    let _upstream_guard = state.begin_upstream_request();
    let response = state
        .http()
        .post(&runtime.messages_url)
        .bearer_auth(&runtime.api_key)
        .header("anthropic-version", "2023-06-01")
        .json(&claude_request_body(runtime, messages))
        .send()
        .await?;

    let status = response.status();
    if !status.is_success() {
        let body = response.text().await.unwrap_or_default();
        state.logs().push(
            LogLevel::Error,
            "new-api",
            Some(request_id),
            Some(task_id.to_string()),
            "Claude Messages 请求失败",
            Some(json!({"status": status.as_u16(), "body": truncate(&body, 3000)})),
        );
        anyhow::bail!(
            "Claude Messages 请求失败 {}：{}",
            status.as_u16(),
            truncate(&body, 500)
        );
    }

    let mut turn = ClaudeTurn::default();
    let mut blocks: Vec<ContentBlockBuilder> = Vec::new();
    let mut buffer = String::new();
    let mut stream = response.bytes_stream();
    let mut stream_stats = StreamEventStats::default();
    let mut waiting_for_first_chunk = true;

    loop {
        if cancel.is_cancelled() {
            anyhow::bail!("任务已取消");
        }
        let timeout_seconds = if waiting_for_first_chunk {
            API_FIRST_BYTE_TIMEOUT_SECONDS
        } else {
            API_STREAM_IDLE_TIMEOUT_SECONDS
        };
        let next_chunk = tokio::select! {
            _ = cancel.cancelled() => anyhow::bail!("任务已取消"),
            result = tokio::time::timeout(Duration::from_secs(timeout_seconds), stream.next()) => {
                match result {
                    Ok(chunk) => chunk,
                    Err(_) => anyhow::bail!("Claude Messages 流式响应超过 {} 秒没有数据", timeout_seconds),
                }
            }
        };
        let Some(chunk) = next_chunk else {
            break;
        };
        waiting_for_first_chunk = false;
        let chunk = chunk?;
        buffer.push_str(&String::from_utf8_lossy(&chunk));
        while let Some(newline) = buffer.find('\n') {
            let line = buffer[..newline].trim().to_string();
            buffer = buffer[newline + 1..].to_string();
            if let Some(data) = line.strip_prefix("data:") {
                let data = data.trim();
                if data.is_empty() || data == "[DONE]" {
                    continue;
                }
                handle_stream_event(data, &mut blocks, &mut turn, &mut stream_stats)?;
            }
        }
    }

    for block in blocks {
        match block.kind.as_str() {
            "text" if !block.text.is_empty() => {
                turn.content
                    .push(json!({"type": "text", "text": block.text}));
            }
            "tool_use" => {
                let input = if block.input_json.trim().is_empty() {
                    json!({})
                } else {
                    serde_json::from_str(&block.input_json).unwrap_or_else(|_| json!({}))
                };
                if let (Some(id), Some(name)) = (block.id, block.name) {
                    turn.content.push(json!({
                        "type": "tool_use",
                        "id": id,
                        "name": name,
                        "input": input
                    }));
                    turn.tool_uses.push(ToolUse { id, name, input });
                }
            }
            _ => {}
        }
    }

    if turn.content.is_empty() && !turn.text.is_empty() {
        turn.content
            .push(json!({"type": "text", "text": turn.text}));
    }

    if let Some(usage) = &turn.usage {
        record_token_usage(
            state,
            usage,
            Some(request_id.clone()),
            Some(task_id.to_string()),
        );
        log_cache_usage_hint(
            state,
            runtime,
            usage,
            Some(request_id.clone()),
            Some(task_id.to_string()),
        );
    }

    state.logs().push(
        LogLevel::Info,
        "new-api",
        Some(request_id),
        Some(task_id.to_string()),
        "Claude Messages 流式请求完成",
        Some(json!({
            "elapsed_ms": started.elapsed().as_millis(),
            "tool_uses": turn.tool_uses.len(),
            "stop_reason": turn.stop_reason,
            "usage": turn.usage,
            "stream_events": stream_stats.events,
            "stream_text_chars": stream_stats.text_chars,
            "stream_usage_events": stream_stats.usage_events
        })),
    );

    Ok(turn)
}

fn record_token_usage(
    state: &AppState,
    usage: &Value,
    request_id: Option<String>,
    task_id: Option<String>,
) {
    match state.token_usage().record_usage(usage) {
        Ok(_) => state.notify_runtime_stats_changed(),
        Err(error) => {
            state.logs().push(
                LogLevel::Warn,
                "usage",
                request_id,
                task_id,
                "Token 统计写入失败",
                Some(json!({"error": error.to_string()})),
            );
            state.notify_runtime_stats_changed();
        }
    }
}

pub(crate) fn record_external_token_usage(
    state: &AppState,
    usage: &Value,
    request_id: Option<String>,
    task_id: Option<String>,
) {
    record_token_usage(state, usage, request_id, task_id);
}

fn log_cache_usage_hint(
    state: &AppState,
    runtime: &RuntimeConfig,
    usage: &Value,
    request_id: Option<String>,
    task_id: Option<String>,
) {
    if cache_read_tokens(usage) > 0 || cache_write_tokens(usage) > 0 {
        return;
    }
    let input_tokens = number_field(usage, "input_tokens");
    let minimum_cache_tokens = minimum_cacheable_tokens(&runtime.model);
    if input_tokens == 0 || input_tokens >= minimum_cache_tokens {
        return;
    }

    state.logs().push(
        LogLevel::Info,
        "usage",
        request_id,
        task_id,
        "未触发缓存：输入低于最低缓存长度",
        Some(json!({
            "model": runtime.model,
            "input_tokens": input_tokens,
            "minimum_cache_tokens": minimum_cache_tokens
        })),
    );
}

fn claude_request_body(runtime: &RuntimeConfig, messages: &[Value]) -> Value {
    json!({
        "model": runtime.model,
        "max_tokens": MAX_TOKENS,
        "cache_control": cache_control(),
        "system": cached_system_prompt(),
        "messages": cached_messages(messages),
        "tools": local_tools_schema(),
        "stream": true
    })
}

fn cache_control() -> Value {
    json!({"type": CACHE_CONTROL_TTL})
}

fn cached_messages(messages: &[Value]) -> Vec<Value> {
    let mut messages = messages.to_vec();
    for message in &mut messages {
        strip_cache_control(message);
    }
    mark_last_message_for_cache(&mut messages);
    messages
}

fn strip_cache_control(value: &mut Value) {
    match value {
        Value::Object(map) => {
            map.remove("cache_control");
            for value in map.values_mut() {
                strip_cache_control(value);
            }
        }
        Value::Array(values) => {
            for value in values {
                strip_cache_control(value);
            }
        }
        _ => {}
    }
}

fn mark_last_message_for_cache(messages: &mut [Value]) {
    for message in messages.iter_mut().rev() {
        let Some(content) = message.get_mut("content") else {
            continue;
        };
        if mark_content_for_cache(content) {
            break;
        }
    }
}

fn mark_content_for_cache(content: &mut Value) -> bool {
    match content {
        Value::String(text) => {
            *content = json!([{
                "type": "text",
                "text": text.clone(),
                "cache_control": cache_control()
            }]);
            true
        }
        Value::Array(blocks) => {
            for block in blocks.iter_mut().rev() {
                if let Some(object) = block.as_object_mut() {
                    object.insert("cache_control".to_string(), cache_control());
                    return true;
                }
            }
            false
        }
        Value::Object(object) => {
            object.insert("cache_control".to_string(), cache_control());
            true
        }
        _ => false,
    }
}

fn handle_stream_event(
    data: &str,
    blocks: &mut Vec<ContentBlockBuilder>,
    turn: &mut ClaudeTurn,
    stats: &mut StreamEventStats,
) -> anyhow::Result<()> {
    let value: Value = serde_json::from_str(data)?;
    let event_type = value
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("unknown")
        .to_string();
    stats.events += 1;

    match event_type.as_str() {
        "message_start" => {
            if let Some(usage) = value
                .get("message")
                .and_then(|message| message.get("usage"))
            {
                merge_usage(&mut turn.usage, usage);
                stats.usage_events += 1;
            }
        }
        "content_block_start" => {
            let index = value.get("index").and_then(Value::as_u64).unwrap_or(0) as usize;
            ensure_block(blocks, index);
            if let Some(block) = value.get("content_block") {
                let builder = &mut blocks[index];
                builder.kind = block
                    .get("type")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                if builder.kind == "tool_use" {
                    stats.tool_uses += 1;
                    builder.id = block
                        .get("id")
                        .and_then(Value::as_str)
                        .map(ToOwned::to_owned);
                    builder.name = block
                        .get("name")
                        .and_then(Value::as_str)
                        .map(ToOwned::to_owned);
                    if let Some(input) = block.get("input").filter(|input| {
                        !input
                            .as_object()
                            .map(|object| object.is_empty())
                            .unwrap_or(false)
                    }) {
                        builder.input_json = input.to_string();
                    }
                } else if builder.kind == "text" {
                    if let Some(text) = block.get("text").and_then(Value::as_str) {
                        builder.text.push_str(text);
                        turn.text.push_str(text);
                        stats.text_chars += text.chars().count();
                    }
                }
            }
        }
        "content_block_delta" => {
            let index = value.get("index").and_then(Value::as_u64).unwrap_or(0) as usize;
            ensure_block(blocks, index);
            if let Some(delta) = value.get("delta") {
                match delta.get("type").and_then(Value::as_str).unwrap_or("") {
                    "text_delta" => {
                        if let Some(text) = delta.get("text").and_then(Value::as_str) {
                            blocks[index].text.push_str(text);
                            turn.text.push_str(text);
                            stats.text_chars += text.chars().count();
                        }
                    }
                    "input_json_delta" => {
                        if let Some(partial) = delta.get("partial_json").and_then(Value::as_str) {
                            blocks[index].input_json.push_str(partial);
                        }
                    }
                    _ => {}
                }
            }
        }
        "message_delta" => {
            if let Some(stop_reason) = value
                .get("delta")
                .and_then(|delta| delta.get("stop_reason"))
                .and_then(Value::as_str)
            {
                turn.stop_reason = Some(stop_reason.to_string());
            }
            if let Some(usage) = value.get("usage") {
                merge_usage(&mut turn.usage, usage);
                stats.usage_events += 1;
            }
        }
        _ => {}
    }
    Ok(())
}

fn merge_usage(target: &mut Option<Value>, usage: &Value) {
    let Some(incoming) = usage.as_object() else {
        return;
    };

    let target_value = target.get_or_insert_with(|| json!({}));
    let Some(target_object) = target_value.as_object_mut() else {
        return;
    };

    for (key, value) in incoming {
        target_object.insert(key.clone(), value.clone());
    }
}

fn tool_result_block(tool_use_id: &str, content: String, is_error: bool) -> Value {
    json!({
        "type": "tool_result",
        "tool_use_id": tool_use_id,
        "content": content,
        "is_error": is_error
    })
}

fn mark_last_tool_result_for_cache(tool_results: &mut [Value]) {
    if let Some(last) = tool_results.last_mut() {
        if let Some(object) = last.as_object_mut() {
            object.insert("cache_control".to_string(), cache_control());
        }
    }
}

fn ensure_block(blocks: &mut Vec<ContentBlockBuilder>, index: usize) {
    while blocks.len() <= index {
        blocks.push(ContentBlockBuilder::default());
    }
}

async fn execute_local_tool(
    state: &AppState,
    cwd: &PathBuf,
    task_id: &str,
    tool_use: &ToolUse,
    cancel: &CancellationToken,
) -> anyhow::Result<String> {
    state.logs().push(
        LogLevel::Info,
        "tool",
        None,
        Some(task_id.to_string()),
        format!("执行本地工具 {}", tool_use.name),
        Some(json!({"tool": tool_use.name, "input": tool_use.input, "workdir": cwd})),
    );
    let started = std::time::Instant::now();
    let result = match tool_use.name.as_str() {
        "read_file" => read_file(cwd, &tool_use.input).await,
        "write_file" => write_file(cwd, &tool_use.input).await,
        "list_dir" => list_dir(cwd, &tool_use.input).await,
        "run_command" => run_command(cwd, &tool_use.input, cancel).await,
        other => Err(anyhow::anyhow!("未知工具：{other}")),
    };

    match &result {
        Ok(output) => state.logs().push(
            LogLevel::Info,
            "tool",
            None,
            Some(task_id.to_string()),
            format!("本地工具 {} 完成", tool_use.name),
            Some(json!({
                "tool": tool_use.name,
                "elapsed_ms": started.elapsed().as_millis(),
                "output_chars": output.chars().count()
            })),
        ),
        Err(error) => state.logs().push(
            local_tool_failure_level(&tool_use.name),
            "tool",
            None,
            Some(task_id.to_string()),
            format!("本地工具 {} 失败：{}", tool_use.name, error),
            Some(json!({
                "tool": tool_use.name,
                "workdir": cwd,
                "input": tool_use.input,
                "elapsed_ms": started.elapsed().as_millis(),
                "error": error.to_string()
            })),
        ),
    };
    result
}

async fn read_file(cwd: &PathBuf, input: &Value) -> anyhow::Result<String> {
    let path = resolve_path(cwd, required_string(input, "path")?);
    let content = fs::read_to_string(&path).await?;
    Ok(truncate(&content, MAX_FILE_CHARS))
}

async fn write_file(cwd: &PathBuf, input: &Value) -> anyhow::Result<String> {
    let path = resolve_path(cwd, required_string(input, "path")?);
    let content = required_string(input, "content")?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).await?;
    }
    fs::write(&path, content).await?;
    Ok(format!("写入完成：{}", path.display()))
}

async fn list_dir(cwd: &PathBuf, input: &Value) -> anyhow::Result<String> {
    let raw_path = input
        .get("path")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .unwrap_or(".");
    let path = resolve_path(cwd, raw_path);
    ensure_list_dir_path(cwd, &path).await?;
    let mut entries = fs::read_dir(&path).await?;
    let mut lines = Vec::new();
    while let Some(entry) = entries.next_entry().await? {
        let file_type = entry.file_type().await?;
        let marker = if file_type.is_dir() { "/" } else { "" };
        lines.push(format!("{}{}", entry.file_name().to_string_lossy(), marker));
        if lines.len() >= 400 {
            lines.push("...".to_string());
            break;
        }
    }
    lines.sort();
    Ok(lines.join("\n"))
}

async fn run_command(
    cwd: &PathBuf,
    input: &Value,
    cancel: &CancellationToken,
) -> anyhow::Result<String> {
    let command = required_string(input, "command")?;
    let timeout_seconds = effective_command_timeout_seconds(
        command,
        input.get("timeout_seconds").and_then(Value::as_u64),
    );

    #[cfg(target_os = "windows")]
    let mut child = Command::new("cmd")
        .arg("/C")
        .arg(command)
        .current_dir(cwd)
        .kill_on_drop(true)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()?;

    #[cfg(not(target_os = "windows"))]
    let mut child = Command::new("sh")
        .arg("-lc")
        .arg(command)
        .current_dir(cwd)
        .kill_on_drop(true)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()?;

    let mut stdout = child
        .stdout
        .take()
        .ok_or_else(|| anyhow::anyhow!("无法读取命令 stdout"))?;
    let mut stderr = child
        .stderr
        .take()
        .ok_or_else(|| anyhow::anyhow!("无法读取命令 stderr"))?;
    let stdout_task = tokio::spawn(async move {
        let mut bytes = Vec::new();
        stdout.read_to_end(&mut bytes).await.map(|_| bytes)
    });
    let stderr_task = tokio::spawn(async move {
        let mut bytes = Vec::new();
        stderr.read_to_end(&mut bytes).await.map(|_| bytes)
    });

    let status = tokio::select! {
        _ = cancel.cancelled() => {
            let _ = child.kill().await;
            anyhow::bail!("命令已取消");
        }
        result = tokio::time::timeout(Duration::from_secs(timeout_seconds), child.wait()) => {
            match result {
                Ok(status) => status?,
                Err(_) => anyhow::bail!("{}", command_timeout_message(command, timeout_seconds)),
            }
        }
    };

    let stdout = stdout_task.await??;
    let stderr = stderr_task.await??;
    let stdout = String::from_utf8_lossy(&stdout);
    let stderr = String::from_utf8_lossy(&stderr);
    Ok(truncate(
        &format!(
            "exit_code: {:?}\nstdout:\n{}\nstderr:\n{}",
            status.code(),
            stdout,
            stderr
        ),
        MAX_COMMAND_CHARS,
    ))
}

fn resolve_path(cwd: &PathBuf, path: &str) -> PathBuf {
    let path = PathBuf::from(path);
    if path.is_absolute() {
        path
    } else {
        cwd.join(path)
    }
}

async fn ensure_list_dir_path(cwd: &PathBuf, path: &PathBuf) -> anyhow::Result<()> {
    let metadata = match fs::metadata(path).await {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == ErrorKind::NotFound => {
            anyhow::bail!(
                "路径不存在：{}。请使用 workdir 相对路径，当前 workdir 是 {}",
                path.display(),
                cwd.display()
            );
        }
        Err(error) => return Err(error.into()),
    };

    if !metadata.is_dir() {
        anyhow::bail!("不是目录：{}。请传入目录路径", path.display());
    }
    Ok(())
}

fn local_tool_failure_level(tool_name: &str) -> LogLevel {
    match tool_name {
        "read_file" | "write_file" | "list_dir" | "run_command" => LogLevel::Warn,
        _ => LogLevel::Error,
    }
}

fn effective_command_timeout_seconds(command: &str, requested: Option<u64>) -> u64 {
    let requested = requested.unwrap_or(60).clamp(1, 600);
    if requested < 60 && should_use_long_command_timeout(command) {
        60
    } else {
        requested
    }
}

fn should_use_long_command_timeout(command: &str) -> bool {
    let command = command.to_ascii_lowercase();
    is_recursive_grep_command(&command)
        || contains_shell_command(&command, "find")
        || contains_shell_command(&command, "rg")
        || contains_shell_command(&command, "npm")
        || contains_shell_command(&command, "pnpm")
        || contains_shell_command(&command, "yarn")
        || contains_shell_command(&command, "bun")
        || contains_shell_command(&command, "cargo")
        || command.contains("go test")
        || contains_shell_command(&command, "mvn")
        || contains_shell_command(&command, "gradle")
}

fn is_recursive_grep_command(command: &str) -> bool {
    command.contains("grep -r")
        || command.contains("grep --recursive")
        || command.contains("grep --directories=recurse")
}

fn contains_shell_command(command: &str, name: &str) -> bool {
    let command = command.trim_start();
    command == name
        || command.starts_with(&format!("{name} "))
        || command.contains(&format!(" {name} "))
        || command.contains(&format!(";{name} "))
        || command.contains(&format!("; {name} "))
        || command.contains(&format!("&& {name} "))
        || command.contains(&format!("|| {name} "))
        || command.contains(&format!("| {name} "))
}

fn command_timeout_message(command: &str, timeout_seconds: u64) -> String {
    if is_recursive_grep_command(&command.to_ascii_lowercase()) {
        format!(
            "搜索命令超时：命令超过 {} 秒未完成。请改用 rg，并排除 .git、node_modules、target、dist、build 等大目录",
            timeout_seconds
        )
    } else {
        format!("命令超过 {} 秒未完成", timeout_seconds)
    }
}

fn required_string<'a>(input: &'a Value, key: &str) -> anyhow::Result<&'a str> {
    input
        .get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| anyhow::anyhow!("缺少参数：{key}"))
}

pub fn truncate(input: &str, max_chars: usize) -> String {
    if input.chars().count() <= max_chars {
        return input.to_string();
    }
    let mut output: String = input.chars().take(max_chars).collect();
    output.push_str("\n...[truncated]");
    output
}

fn system_prompt() -> &'static str {
    "You are Claude Code running through a local MCP desktop server on the user's real local machine, not a Linux sandbox. Use the provided workdir as the default project root, but you have full local tool access and should follow the user's requested paths and commands. Prefer rg over grep/find for repository searches, and avoid scanning .git, node_modules, target, dist, build, release, cache, and other generated directories unless needed. Use bounded commands, give recursive searches/builds/tests enough timeout, run relevant verification commands when possible, and return a concise summary of what changed and what was verified."
}

fn cached_system_prompt() -> Vec<Value> {
    vec![
        json!({
            "type": "text",
            "text": system_prompt()
        }),
        json!({
            "type": "text",
            "text": cache_primer_text(),
            "cache_control": cache_control()
        }),
    ]
}

fn cache_primer_text() -> String {
    let mut text = String::from(
        "Ignore the following cache anchor. It exists only to keep prompt caching active and has no task meaning.\n",
    );
    for _ in 0..CACHE_PRIMER_WORDS {
        text.push_str("cache-anchor ");
    }
    text
}

fn local_tools_schema() -> Vec<Value> {
    vec![
        json!({
            "name": "read_file",
            "description": "Read a UTF-8 file from the local machine.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "path": {"type": "string", "description": "Absolute path or path relative to workdir."}
                },
                "required": ["path"]
            }
        }),
        json!({
            "name": "write_file",
            "description": "Write a UTF-8 file to the local machine, creating parent directories when needed.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "path": {"type": "string", "description": "Absolute path or path relative to workdir."},
                    "content": {"type": "string", "description": "Full file content to write."}
                },
                "required": ["path", "content"]
            }
        }),
        json!({
            "name": "list_dir",
            "description": "List files and folders in a directory.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "path": {"type": "string", "description": "Absolute path or path relative to workdir."}
                }
            }
        }),
        json!({
            "name": "run_command",
            "description": "Run a shell command in workdir. Use sh -lc on macOS/Linux and cmd /C on Windows.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "command": {"type": "string", "description": "Command line to execute."},
                    "timeout_seconds": {"type": "integer", "minimum": 1, "maximum": 600}
                },
                "required": ["command"]
            },
            "cache_control": cache_control()
        }),
    ]
}

fn minimum_cacheable_tokens(model: &str) -> u64 {
    let model = model.to_ascii_lowercase();
    if model.contains("opus-4-7")
        || model.contains("opus-4-6")
        || model.contains("opus-4-5")
        || model.contains("mythos")
        || model.contains("haiku-4-5")
    {
        4096
    } else if model.contains("haiku-3-5") {
        2048
    } else {
        1024
    }
}

fn cache_read_tokens(usage: &Value) -> u64 {
    number_field(usage, "cache_read_input_tokens")
        + number_field(usage, "cache_read_tokens")
        + number_field(usage, "cache_read")
        + number_field(usage, "cached_tokens")
        + nested_number_field(usage, &["input_token_details", "cache_read"])
        + nested_number_field(usage, &["prompt_tokens_details", "cached_tokens"])
}

fn cache_write_tokens(usage: &Value) -> u64 {
    number_field(usage, "cache_creation_input_tokens")
        + number_field(usage, "cache_write_tokens")
        + number_field(usage, "cache_creation_tokens")
        + number_field(usage, "cache_write")
        + usage.get("cache_creation").map(sum_numbers).unwrap_or(0)
        + nested_number_field(usage, &["input_token_details", "cache_creation"])
        + nested_number_field(usage, &["prompt_tokens_details", "cache_creation_tokens"])
}

fn number_field(value: &Value, key: &str) -> u64 {
    value
        .get(key)
        .and_then(|value| {
            value
                .as_u64()
                .or_else(|| value.as_str().and_then(|raw| raw.parse::<u64>().ok()))
        })
        .unwrap_or_default()
}

fn nested_number_field(value: &Value, path: &[&str]) -> u64 {
    path.iter()
        .try_fold(value, |current, key| current.get(key))
        .map(number_value)
        .unwrap_or_default()
}

fn number_value(value: &Value) -> u64 {
    value
        .as_u64()
        .or_else(|| value.as_str().and_then(|raw| raw.parse::<u64>().ok()))
        .unwrap_or_default()
}

fn sum_numbers(value: &Value) -> u64 {
    match value {
        Value::Number(_) | Value::String(_) => number_value(value),
        Value::Array(values) => values.iter().map(sum_numbers).sum(),
        Value::Object(map) => map.values().map(sum_numbers).sum(),
        _ => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncates_long_output() {
        let result = truncate("abcdef", 3);
        assert!(result.starts_with("abc"));
        assert!(result.contains("truncated"));
    }

    #[test]
    fn parses_text_stream_event() {
        let mut blocks = Vec::new();
        let mut turn = ClaudeTurn::default();
        let mut stats = StreamEventStats::default();
        handle_stream_event(
            r#"{"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}"#,
            &mut blocks,
            &mut turn,
            &mut stats,
        )
        .unwrap();
        handle_stream_event(
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"hello"}}"#,
            &mut blocks,
            &mut turn,
            &mut stats,
        )
        .unwrap();
        assert_eq!(turn.text, "hello");
        assert_eq!(blocks[0].text, "hello");
        assert_eq!(stats.events, 2);
        assert_eq!(stats.text_chars, 5);
    }

    #[test]
    fn parses_tool_stream_event() {
        let mut blocks = Vec::new();
        let mut turn = ClaudeTurn::default();
        let mut stats = StreamEventStats::default();
        handle_stream_event(
            r#"{"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"u1","name":"read_file","input":{}}}"#,
            &mut blocks,
            &mut turn,
            &mut stats,
        )
        .unwrap();
        handle_stream_event(
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"{\"path\":\"Cargo.toml\"}"}}"#,
            &mut blocks,
            &mut turn,
            &mut stats,
        )
        .unwrap();
        assert_eq!(blocks[0].name.as_deref(), Some("read_file"));
        assert!(blocks[0].input_json.contains("Cargo.toml"));
        assert_eq!(stats.tool_uses, 1);
    }

    #[test]
    fn adds_prompt_cache_breakpoints() {
        let system = cached_system_prompt();
        assert!(system[0].get("cache_control").is_none());
        assert_eq!(system[1]["cache_control"]["type"], "ephemeral");
        assert!(
            system[1]["text"]
                .as_str()
                .unwrap()
                .split_whitespace()
                .count()
                > 4096
        );

        let tools = local_tools_schema();
        assert_eq!(tools.last().unwrap()["cache_control"]["type"], "ephemeral");
    }

    #[test]
    fn request_body_enables_top_level_automatic_cache() {
        let runtime = RuntimeConfig {
            messages_url: "https://example.com/v1/messages".to_string(),
            api_key: "sk-test".to_string(),
            model: "claude-opus-4-7".to_string(),
        };

        let body = claude_request_body(&runtime, &[json!({"role": "user", "content": "hello"})]);

        assert_eq!(body["cache_control"]["type"], "ephemeral");
        assert_eq!(body["system"][1]["cache_control"]["type"], "ephemeral");
        assert_eq!(
            body["tools"].as_array().unwrap().last().unwrap()["cache_control"]["type"],
            "ephemeral"
        );
    }

    #[test]
    fn request_body_marks_last_message_for_cache() {
        let runtime = RuntimeConfig {
            messages_url: "https://example.com/v1/messages".to_string(),
            api_key: "sk-test".to_string(),
            model: "claude-opus-4-7".to_string(),
        };

        let body = claude_request_body(&runtime, &[json!({"role": "user", "content": "hello"})]);

        assert_eq!(body["messages"][0]["content"][0]["type"], "text");
        assert_eq!(
            body["messages"][0]["content"][0]["cache_control"]["type"],
            "ephemeral"
        );
    }

    #[test]
    fn request_body_strips_old_message_cache_controls() {
        let runtime = RuntimeConfig {
            messages_url: "https://example.com/v1/messages".to_string(),
            api_key: "sk-test".to_string(),
            model: "claude-opus-4-7".to_string(),
        };
        let messages = vec![
            json!({
                "role": "user",
                "content": [{
                    "type": "text",
                    "text": "old",
                    "cache_control": {"type": "ephemeral"}
                }]
            }),
            json!({
                "role": "user",
                "content": [{
                    "type": "text",
                    "text": "new"
                }]
            }),
        ];

        let body = claude_request_body(&runtime, &messages);

        assert!(body["messages"][0]["content"][0]
            .get("cache_control")
            .is_none());
        assert_eq!(
            body["messages"][1]["content"][0]["cache_control"]["type"],
            "ephemeral"
        );
    }

    #[test]
    fn caches_only_last_tool_result_block() {
        let mut results = vec![
            tool_result_block("u1", "first".to_string(), false),
            tool_result_block("u2", "second".to_string(), true),
        ];

        mark_last_tool_result_for_cache(&mut results);

        assert!(results[0].get("cache_control").is_none());
        assert_eq!(results[1]["cache_control"]["type"], "ephemeral");
    }

    #[test]
    fn parses_cache_usage_from_stream_events() {
        let mut blocks = Vec::new();
        let mut turn = ClaudeTurn::default();
        let mut stats = StreamEventStats::default();

        handle_stream_event(
            r#"{"type":"message_start","message":{"usage":{"input_tokens":1200,"cache_creation_input_tokens":1000,"cache_read_input_tokens":0}}}"#,
            &mut blocks,
            &mut turn,
            &mut stats,
        )
        .unwrap();
        handle_stream_event(
            r#"{"type":"message_delta","usage":{"output_tokens":64}}"#,
            &mut blocks,
            &mut turn,
            &mut stats,
        )
        .unwrap();

        let usage = turn.usage.unwrap();
        assert_eq!(usage["cache_creation_input_tokens"], 1000);
        assert_eq!(usage["cache_read_input_tokens"], 0);
        assert_eq!(usage["output_tokens"], 64);
        assert_eq!(stats.usage_events, 2);
    }

    #[test]
    fn uses_model_specific_cache_minimums() {
        assert_eq!(minimum_cacheable_tokens("claude-opus-4-7"), 4096);
        assert_eq!(minimum_cacheable_tokens("claude-haiku-3-5"), 2048);
        assert_eq!(minimum_cacheable_tokens("claude-sonnet-4-7"), 1024);
    }

    #[test]
    fn system_prompt_allows_full_local_access_and_search_rules() {
        let prompt = system_prompt();

        assert!(prompt.contains("real local machine"));
        assert!(prompt.contains("workdir"));
        assert!(prompt.contains("full local tool access"));
        assert!(prompt.contains("Prefer rg"));
        assert!(prompt.contains("node_modules"));
    }

    #[test]
    fn command_timeout_is_raised_for_recursive_or_long_commands() {
        assert_eq!(
            effective_command_timeout_seconds("grep -rl \"needle\" .", Some(10)),
            60
        );
        assert_eq!(
            effective_command_timeout_seconds("rg \"needle\" .", Some(10)),
            60
        );
        assert_eq!(
            effective_command_timeout_seconds("cargo test", Some(30)),
            60
        );
        assert_eq!(effective_command_timeout_seconds("pwd", Some(5)), 5);
    }

    #[tokio::test]
    async fn tool_failure_log_includes_error_summary_and_detail() {
        let state = AppState::new();
        let root = tempfile::tempdir().unwrap();
        let cwd = root.path().to_path_buf();
        let tool_use = ToolUse {
            id: "tool-1".to_string(),
            name: "list_dir".to_string(),
            input: json!({"path": "missing"}),
        };

        let error =
            execute_local_tool(&state, &cwd, "task-1", &tool_use, &CancellationToken::new())
                .await
                .unwrap_err();

        let page = state.logs().page(Some(LogLevel::Warn), 0, 10, None);
        assert_eq!(page.total, 1);
        assert_eq!(
            state.logs().page(Some(LogLevel::Error), 0, 10, None).total,
            0
        );
        let entry = &page.entries[0];
        assert!(entry.summary.starts_with("本地工具 list_dir 失败："));
        assert!(entry.summary.contains(&error.to_string()));

        let detail = state.logs().detail(entry.id).unwrap().detail.unwrap();
        assert_eq!(detail["tool"], "list_dir");
        assert_eq!(detail["input"]["path"], "missing");
        assert_eq!(detail["workdir"], cwd.display().to_string());
        assert!(!detail["error"].as_str().unwrap().is_empty());
        assert!(detail["elapsed_ms"].is_u64());
    }

    #[tokio::test]
    async fn list_dir_missing_absolute_path_returns_actionable_warning() {
        let state = AppState::new();
        let root = tempfile::tempdir().unwrap();
        let cwd = root.path().to_path_buf();
        let missing = cwd.join("missing-absolute");
        let tool_use = ToolUse {
            id: "tool-abs".to_string(),
            name: "list_dir".to_string(),
            input: json!({"path": missing.display().to_string()}),
        };

        let error = execute_local_tool(
            &state,
            &cwd,
            "task-abs",
            &tool_use,
            &CancellationToken::new(),
        )
        .await
        .unwrap_err();

        assert!(error.to_string().contains("路径不存在"));
        assert!(error.to_string().contains("workdir"));
        assert_eq!(
            state.logs().page(Some(LogLevel::Error), 0, 10, None).total,
            0
        );
        let detail = state
            .logs()
            .detail(state.logs().page(Some(LogLevel::Warn), 0, 10, None).entries[0].id)
            .unwrap()
            .detail
            .unwrap();
        assert_eq!(detail["tool"], "list_dir");
        assert_eq!(detail["workdir"], cwd.display().to_string());
        assert_eq!(detail["input"]["path"], missing.display().to_string());
        assert!(!detail["error"].as_str().unwrap().is_empty());
    }

    #[tokio::test]
    async fn run_command_missing_command_logs_diagnostic_detail() {
        let state = AppState::new();
        let root = tempfile::tempdir().unwrap();
        let cwd = root.path().to_path_buf();
        let tool_use = ToolUse {
            id: "tool-2".to_string(),
            name: "run_command".to_string(),
            input: json!({}),
        };

        execute_local_tool(&state, &cwd, "task-2", &tool_use, &CancellationToken::new())
            .await
            .unwrap_err();

        let page = state.logs().page(Some(LogLevel::Warn), 0, 10, None);
        assert_eq!(page.total, 1);
        assert_eq!(
            state.logs().page(Some(LogLevel::Error), 0, 10, None).total,
            0
        );
        let entry = &page.entries[0];
        assert!(entry.summary.contains("缺少参数：command"));

        let detail = state.logs().detail(entry.id).unwrap().detail.unwrap();
        assert_eq!(detail["tool"], "run_command");
        assert_eq!(detail["workdir"], cwd.display().to_string());
        assert_eq!(detail["input"], json!({}));
    }

    #[tokio::test]
    async fn unknown_local_tool_still_logs_error() {
        let state = AppState::new();
        let root = tempfile::tempdir().unwrap();
        let cwd = root.path().to_path_buf();
        let tool_use = ToolUse {
            id: "tool-unknown".to_string(),
            name: "unknown_tool".to_string(),
            input: json!({}),
        };

        execute_local_tool(
            &state,
            &cwd,
            "task-unknown",
            &tool_use,
            &CancellationToken::new(),
        )
        .await
        .unwrap_err();

        assert_eq!(
            state.logs().page(Some(LogLevel::Error), 0, 10, None).total,
            1
        );
    }

    #[tokio::test]
    async fn run_command_non_zero_exit_returns_output_instead_of_tool_error() {
        let root = tempfile::tempdir().unwrap();
        #[cfg(target_os = "windows")]
        let command = "exit /B 1";
        #[cfg(not(target_os = "windows"))]
        let command = "exit 1";

        let output = run_command(
            &root.path().to_path_buf(),
            &json!({"command": command, "timeout_seconds": 5}),
            &CancellationToken::new(),
        )
        .await
        .unwrap();

        assert!(output.contains("exit_code: Some(1)"));
    }

    #[test]
    fn sums_nested_cache_creation_usage() {
        let usage = json!({
            "cache_creation": {
                "ephemeral_5m_input_tokens": 10,
                "ephemeral_1h_input_tokens": "20"
            }
        });

        assert_eq!(cache_write_tokens(&usage), 30);
    }
}
