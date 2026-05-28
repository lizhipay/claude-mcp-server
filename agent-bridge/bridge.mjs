#!/usr/bin/env node

import fs from "node:fs";
import path from "node:path";
import readline from "node:readline";
import { createRequire } from "node:module";
import { query } from "@anthropic-ai/claude-agent-sdk";

const require = createRequire(import.meta.url);
const activeJobs = new Map();
const SDK_VERSION = readSdkVersion();
const NATIVE_BINARY_PATH = resolveNativeBinaryPath();
const CLIENT_APP = "claude-mcp/agent-bridge";
const FIRST_RESPONSE_TIMEOUT_MS = readPositiveIntegerEnv("CLAUDE_MCP_FIRST_RESPONSE_TIMEOUT_MS", 0);

emit({
  type: "ready",
  sdk_version: SDK_VERSION,
  native_binary_path: NATIVE_BINARY_PATH,
  node: process.version,
  platform: process.platform,
  arch: process.arch,
});

const rl = readline.createInterface({
  input: process.stdin,
  crlfDelay: Infinity,
});

rl.on("line", (line) => {
  const trimmed = line.trim();
  if (!trimmed) return;
  let command;
  try {
    command = JSON.parse(trimmed);
  } catch (error) {
    emit({ type: "bridge_error", summary: "无法解析 bridge 命令", error: String(error) });
    return;
  }

  if (command.type === "start") {
    startJob(command).catch((error) => finishWithError(command.job_id, error));
  } else if (command.type === "cancel") {
    cancelJob(command.job_id);
  } else if (command.type === "status") {
    emit({
      type: "status_response",
      request_id: command.request_id,
      sdk_version: SDK_VERSION,
      native_binary_path: NATIVE_BINARY_PATH,
      active_jobs: activeJobs.size,
      waiting_first_response: countWaitingFirstResponse(),
      node: process.version,
      platform: process.platform,
      arch: process.arch,
    });
  } else if (command.type === "shutdown") {
    for (const job of activeJobs.values()) {
      job.abortController.abort();
    }
    process.exit(0);
  }
});

process.on("SIGTERM", () => {
  for (const job of activeJobs.values()) {
    job.abortController.abort();
  }
  process.exit(0);
});

async function startJob(command) {
  const jobId = requiredString(command.job_id, "job_id");
  if (activeJobs.has(jobId)) {
    emit({ type: "error", job_id: jobId, error: "任务已经在运行" });
    return;
  }

  const prompt = requiredString(command.prompt, "prompt");
  const cwd = requiredString(command.cwd, "cwd");
  const model = requiredString(command.model, "model");
  const apiKey = requiredString(command.api_key, "api_key");
  const baseUrl = normalizeBaseUrl(requiredString(command.base_url, "base_url"));
  const resumeSessionId = optionalString(command.resume_session_id);
  const abortController = new AbortController();
  const startedAt = Date.now();
  activeJobs.set(jobId, {
    abortController,
    query: null,
    startedAt,
    firstResponseAt: null,
    firstResponseSeen: false,
    firstResponseTimer: null,
    textChars: 0,
    toolCalls: 0,
    partialEvents: 0,
    lastSummaryAt: 0,
  });
  armFirstResponseTimeout(jobId);

  emit({
    type: "started",
    job_id: jobId,
    summary: "Agent SDK 任务已启动",
    detail: {
      cwd,
      model,
      base_url: baseUrl,
      permission_mode: "bypassPermissions",
      resume_session_id: resumeSessionId,
      active_jobs: activeJobs.size,
      waiting_first_response: countWaitingFirstResponse(),
      started_at: startedAt,
    },
  });

  const sessionQuery = query({
    prompt,
    options: {
      abortController,
      cwd,
      model,
      resume: resumeSessionId || undefined,
      tools: { type: "preset", preset: "claude_code" },
      permissionMode: "bypassPermissions",
      allowDangerouslySkipPermissions: true,
      includePartialMessages: true,
      includeHookEvents: true,
      forwardSubagentText: false,
      agentProgressSummaries: true,
      persistSession: true,
      settingSources: ["project", "local"],
      pathToClaudeCodeExecutable: NATIVE_BINARY_PATH || undefined,
      systemPrompt: {
        type: "preset",
        preset: "claude_code",
        append:
          "You are running inside Claude MCP. Treat cwd as the project root. Prefer bounded searches, avoid generated directories, and return a concise verified summary.",
      },
      env: {
        ...process.env,
        ANTHROPIC_API_KEY: apiKey,
        ANTHROPIC_BASE_URL: baseUrl,
        CLAUDE_AGENT_SDK_CLIENT_APP: CLIENT_APP,
      },
      stderr: (data) => {
        const text = String(data || "").trim();
        if (text) {
          emit({
            type: "log",
            job_id: jobId,
            level: "debug",
            source: "agent-sdk",
            summary: "Agent SDK stderr",
            detail: { stderr: truncate(text, 4000) },
          });
        }
      },
    },
  });

  activeJobs.get(jobId).query = sessionQuery;

  try {
    for await (const message of sessionQuery) {
      handleSdkMessage(jobId, message);
    }
    if (activeJobs.has(jobId)) {
      finishWithError(jobId, new Error("Agent SDK 结束时没有返回 result"));
    }
  } catch (error) {
    if (!activeJobs.has(jobId)) return;
    if (abortController.signal.aborted) {
      emit({
        type: "cancelled",
        job_id: jobId,
        error: "任务已取消",
        detail: { elapsed_ms: Date.now() - startedAt },
      });
      finishJob(jobId);
      return;
    }
    finishWithError(jobId, error);
  }
}

function handleSdkMessage(jobId, message) {
  const job = activeJobs.get(jobId);
  if (!job) return;

  if (message?.type === "result") {
    markFirstResponse(jobId);
    if (message.usage) {
      emit({
        type: "usage",
        job_id: jobId,
        usage: message.usage,
        detail: { model_usage: message.modelUsage, ...jobTimingDetail(jobId) },
      });
    }
    if (message.subtype === "success") {
      emit({
        type: "done",
        job_id: jobId,
        output: message.result || "",
        session_id: message.session_id,
        summary: "Agent SDK 任务完成",
        detail: { ...resultDetail(message), ...jobTimingDetail(jobId) },
      });
    } else {
      emit({
        type: "error",
        job_id: jobId,
        error: (message.errors || []).join("\n") || message.subtype || "Agent SDK 执行失败",
        session_id: message.session_id,
        detail: { ...resultDetail(message), ...jobTimingDetail(jobId) },
      });
    }
    finishJob(jobId);
    return;
  }

  if (message?.type === "system") {
    handleSystemMessage(jobId, message);
    return;
  }

  const text = extractText(message);
  if (text) {
    markFirstResponse(jobId);
    job.textChars += text.length;
    job.partialEvents += 1;
    const now = Date.now();
    if (now - job.lastSummaryAt >= 1000) {
      job.lastSummaryAt = now;
      emit({
        type: "stream_summary",
        job_id: jobId,
        summary: "Agent SDK 正在返回内容",
        detail: {
          text_chars: job.textChars,
          partial_events: job.partialEvents,
          preview: truncate(text, 600),
          ...jobTimingDetail(jobId),
        },
      });
    }
  }

  const toolNames = extractToolNames(message);
  if (toolNames.length > 0) {
    markFirstResponse(jobId);
    job.toolCalls += toolNames.length;
    emit({
      type: "log",
      job_id: jobId,
      level: "info",
      source: "agent-sdk",
      summary: `Agent SDK 调用工具：${toolNames.join(", ")}`,
      detail: { tools: toolNames, total_tool_calls: job.toolCalls, ...jobTimingDetail(jobId) },
    });
  }
}

function handleSystemMessage(jobId, message) {
  if (message.subtype === "init") {
    emit({
      type: "init",
      job_id: jobId,
      session_id: message.session_id,
      summary: "Agent SDK session 已初始化",
      detail: {
        cwd: message.cwd,
        model: message.model,
        tools: message.tools,
        mcp_servers: message.mcp_servers,
        permission_mode: message.permissionMode,
        claude_code_version: message.claude_code_version,
        ...jobTimingDetail(jobId),
      },
    });
    return;
  }

  if (message.subtype === "permission_denied") {
    markFirstResponse(jobId);
    emit({
      type: "permission_denied",
      job_id: jobId,
      session_id: message.session_id,
      summary: `权限已拒绝：${message.tool_name || "unknown"}`,
      detail: redactSecrets(message),
    });
    return;
  }

  if (
    message.subtype === "task_started" ||
    message.subtype === "task_progress" ||
    message.subtype === "task_notification"
  ) {
    emit({
      type: "log",
      job_id: jobId,
      session_id: message.session_id,
      level: message.status === "failed" ? "warn" : "info",
      source: "agent-sdk",
      summary: message.summary || message.description || `Agent SDK ${message.subtype}`,
      detail: { ...redactSecrets(message), ...jobTimingDetail(jobId) },
    });
  }
}

function cancelJob(jobId) {
  const job = activeJobs.get(jobId);
  if (!job) {
    emit({ type: "cancelled", job_id: jobId, error: "任务不存在或已经结束" });
    return;
  }
  job.abortController.abort();
  if (job.query && typeof job.query.close === "function") {
    job.query.close();
  }
  emit({
    type: "cancelled",
    job_id: jobId,
    error: "任务已取消",
  });
  finishJob(jobId);
}

function finishWithError(jobId, error, options = {}) {
  if (!jobId) {
    emit({ type: "bridge_error", error: String(error) });
    return;
  }
  const job = activeJobs.get(jobId);
  if (job && options.abort) {
    job.abortController.abort();
    if (job.query && typeof job.query.close === "function") {
      job.query.close();
    }
  }
  emit({
    type: "error",
    job_id: jobId,
    summary: options.summary,
    error: error?.stack || error?.message || String(error),
    detail: jobTimingDetail(jobId),
  });
  finishJob(jobId);
}

function armFirstResponseTimeout(jobId) {
  if (FIRST_RESPONSE_TIMEOUT_MS <= 0) return;
  const job = activeJobs.get(jobId);
  if (!job) return;
  job.firstResponseTimer = setTimeout(() => {
    const current = activeJobs.get(jobId);
    if (!current || current.firstResponseSeen) return;
    emit({
      type: "log",
      job_id: jobId,
      level: "warn",
      source: "agent-sdk",
      summary: "Agent SDK 上游首包等待较久",
      detail: {
        first_response_warn_ms: FIRST_RESPONSE_TIMEOUT_MS,
        ...jobTimingDetail(jobId),
      },
    });
    current.firstResponseTimer = null;
  }, FIRST_RESPONSE_TIMEOUT_MS);
  job.firstResponseTimer.unref?.();
}

function markFirstResponse(jobId) {
  const job = activeJobs.get(jobId);
  if (!job || job.firstResponseSeen) return;
  job.firstResponseSeen = true;
  job.firstResponseAt = Date.now();
  if (job.firstResponseTimer) {
    clearTimeout(job.firstResponseTimer);
    job.firstResponseTimer = null;
  }
}

function finishJob(jobId) {
  const job = activeJobs.get(jobId);
  if (job?.firstResponseTimer) clearTimeout(job.firstResponseTimer);
  activeJobs.delete(jobId);
  emit({ type: "status_update", job_id: jobId });
}

function countWaitingFirstResponse() {
  let count = 0;
  for (const job of activeJobs.values()) {
    if (!job.firstResponseSeen) count += 1;
  }
  return count;
}

function jobTimingDetail(jobId) {
  const job = activeJobs.get(jobId);
  if (!job) {
    return {
      active_jobs: activeJobs.size,
      waiting_first_response: countWaitingFirstResponse(),
    };
  }
  const firstResponseWaitMs = job.firstResponseAt
    ? job.firstResponseAt - job.startedAt
    : Date.now() - job.startedAt;
  return {
    active_jobs: activeJobs.size,
    waiting_first_response: countWaitingFirstResponse(),
    started_at: job.startedAt,
    first_response_at: job.firstResponseAt,
    first_response_wait_ms: firstResponseWaitMs,
  };
}

function emit(event) {
  const enriched = {
    active_jobs: activeJobs.size,
    waiting_first_response: countWaitingFirstResponse(),
    ...event,
  };
  process.stdout.write(`${JSON.stringify(enriched)}\n`);
}

function requiredString(value, name) {
  if (typeof value !== "string" || value.trim() === "") {
    throw new Error(`缺少参数：${name}`);
  }
  return value;
}

function optionalString(value) {
  if (typeof value !== "string") return undefined;
  const trimmed = value.trim();
  return trimmed ? trimmed : undefined;
}

function readPositiveIntegerEnv(name, fallback) {
  const raw = process.env[name];
  if (raw === undefined || raw.trim() === "") return fallback;
  const parsed = Number.parseInt(raw, 10);
  return Number.isFinite(parsed) && parsed >= 0 ? parsed : fallback;
}

function normalizeBaseUrl(input) {
  return input.trim().replace(/\/+$/, "").replace(/\/v1\/messages$/, "");
}

function readSdkVersion() {
  try {
    const sdkPath = require.resolve("@anthropic-ai/claude-agent-sdk");
    const packagePath = path.join(path.dirname(sdkPath), "package.json");
    return JSON.parse(fs.readFileSync(packagePath, "utf8")).version || "unknown";
  } catch {
    return "unknown";
  }
}

function resolveNativeBinaryPath() {
  const candidates = nativePackageCandidates();
  for (const packageName of candidates) {
    for (const binaryName of process.platform === "win32" ? ["claude.exe"] : ["claude"]) {
      try {
        return require.resolve(`${packageName}/${binaryName}`);
      } catch {
        // Try the next package.
      }
    }
  }
  return null;
}

function nativePackageCandidates() {
  const arch = process.arch === "arm64" ? "arm64" : process.arch === "x64" ? "x64" : process.arch;
  if (process.platform === "linux") {
    return [
      `@anthropic-ai/claude-agent-sdk-linux-${arch}`,
      `@anthropic-ai/claude-agent-sdk-linux-${arch}-musl`,
    ];
  }
  return [`@anthropic-ai/claude-agent-sdk-${process.platform}-${arch}`];
}

function extractText(message) {
  const blocks = message?.message?.content || message?.content || [];
  if (!Array.isArray(blocks)) return "";
  return blocks
    .filter((block) => block?.type === "text" && typeof block.text === "string")
    .map((block) => block.text)
    .join("");
}

function extractToolNames(message) {
  const blocks = message?.message?.content || message?.content || [];
  if (!Array.isArray(blocks)) return [];
  return blocks
    .filter((block) => block?.type === "tool_use" && typeof block.name === "string")
    .map((block) => block.name);
}

function resultDetail(message) {
  return {
    subtype: message.subtype,
    duration_ms: message.duration_ms,
    duration_api_ms: message.duration_api_ms,
    num_turns: message.num_turns,
    total_cost_usd: message.total_cost_usd,
    stop_reason: message.stop_reason,
    usage: message.usage,
    model_usage: message.modelUsage,
    permission_denials: message.permission_denials,
    errors: message.errors,
  };
}

function redactSecrets(value) {
  if (Array.isArray(value)) return value.map(redactSecrets);
  if (!value || typeof value !== "object") return value;
  const output = {};
  for (const [key, item] of Object.entries(value)) {
    if (/api[_-]?key|authorization|token|password|secret|cookie/i.test(key)) {
      output[key] = "***";
    } else {
      output[key] = redactSecrets(item);
    }
  }
  return output;
}

function truncate(value, maxChars) {
  const text = String(value || "");
  return text.length <= maxChars ? text : `${text.slice(0, maxChars)}\n...[truncated]`;
}
