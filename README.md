# Claude MCP

Claude MCP 是一个跨平台桌面软件，用 Tauri 2、Rust、React 和 TypeScript 构建。它在本机启动一个 MCP Streamable HTTP 服务，让 Codex 等 MCP 客户端可以在不安装 Claude Code CLI 的情况下，把任务交给 Claude 兼容的 `/v1/messages` API 执行。

默认 MCP 地址是：

```text
http://127.0.0.1:8765/mcp
```

## 适合做什么

- 让 Codex 调用一个本地 Claude Agent。
- 让 Claude 读写指定工作目录里的文件。
- 让 Claude 在指定工作目录执行命令。
- 查看每次 MCP 请求、上游 API 调用、本地工具执行和错误日志。
- 统计每天的 input、output、cache read、cache write 和 total token。

## 下载和安装

安装包在 GitHub Releases：

[https://github.com/lizhipay/claude-mcp-server/releases](https://github.com/lizhipay/claude-mcp-server/releases)

按系统选择安装包：

| 系统 | 架构 | 文件 |
| --- | --- | --- |
| macOS | Apple Silicon | `Claude.MCP_*_aarch64.dmg` |
| macOS | Intel | `Claude.MCP_*_x64.dmg` |
| Windows | x64 | `Claude.MCP_*_x64-setup.exe` |
| Windows | ARM64 | `Claude.MCP_*_arm64-setup.exe` |
| Linux | x64 | `Claude.MCP_*_amd64.AppImage` 或 `Claude.MCP_*_amd64.deb` |
| Linux | ARM64 | `Claude.MCP_*_aarch64.AppImage` 或 `Claude.MCP_*_arm64.deb` |

macOS 当前使用临时签名，没有 Apple Developer ID 公证。如果系统提示无法验证开发者，可以右键打开；如果仍被拦截，可以执行：

```bash
xattr -dr com.apple.quarantine "/Applications/Claude MCP.app"
```

## 首次配置

打开 Claude MCP 后，在主控台填写：

| 字段 | 示例 | 说明 |
| --- | --- | --- |
| API 地址 | `https://api.anthropic.com` | 可以填根地址，也可以填完整 `/v1/messages` |
| API Key | `sk-ant-...` | 保存到本机应用配置文件，不会在界面和日志里明文展示 |
| 模型名称 | `claude-opus-4-7` | 默认值就是 `claude-opus-4-7` |
| 端口 | `8765` | 服务只监听 `127.0.0.1` |

保存后点击“测试连接”。测试通过后点击“启动服务”，主控台会显示：

```text
http://127.0.0.1:8765/mcp
```

健康检查地址是：

```text
http://127.0.0.1:8765/health
```

## 在 Codex 里添加 MCP

启动 Claude MCP 服务后，在终端执行：

```bash
codex mcp add claude-mcp --url http://127.0.0.1:8765/mcp
```

检查是否添加成功：

```bash
codex mcp list
codex mcp get claude-mcp
```

也可以手动写入 `~/.codex/config.toml`：

```toml
[mcp_servers.claude-mcp]
url = "http://127.0.0.1:8765/mcp"
enabled = true
```

添加后重新打开一个 Codex 线程。如果 MCP 连接成功，Codex 里会出现类似下面的工具名：

```text
mcp__claude_mcp__code
mcp__claude_mcp__code_start
mcp__claude_mcp__code_status
mcp__claude_mcp__code_result
```

实际显示名称由 Codex 决定，但工具本体来自 `claude-mcp`。

## MCP 工具

Claude MCP 对外提供这些工具：

| 工具 | 用途 |
| --- | --- |
| `code` | 同步执行一个任务；超过约 90 秒会返回 `job_id` |
| `code_with_context` | 带指定文件内容执行任务 |
| `code_start` | 启动异步任务，立刻返回 `job_id` |
| `code_with_context_start` | 带指定文件内容启动异步任务 |
| `code_async` | `code_start` 的别名 |
| `code_with_context_async` | `code_with_context_start` 的别名 |
| `code_status` | 查询任务状态和最近输出 |
| `code_result` | 读取已完成任务的最终结果 |
| `code_wait` | 等待单个任务完成，超时才返回当前状态 |
| `code_batch_wait` | 一次等待多个任务完成，减少频繁轮询 |
| `code_batch_result` | 一次读取多个任务结果，不等待 |
| `code_batch_poll` | 增量读取多个任务的新完成结果 |
| `code_cancel` | 取消排队中或运行中的任务 |

推荐用法：

- 短任务直接用 `code`。
- 长任务用 `code_start` 启动，再用 `code_wait` 等待结果。
- 多任务并发用多个 `code_start` 启动；想一次等完用 `code_batch_wait`，想边完成边处理用 `code_batch_poll`。
- 不建议让 Codex 每隔几秒反复调用 `code_status`，除非只是临时查看进度。

任务状态包括：

```text
queued
running
succeeded
failed
cancelled
```

`code` 和 `code_start` 参数：

```json
{
  "prompt": "要交给 Claude 执行的任务",
  "workdir": "/Users/zoe/Developer/project"
}
```

`code_with_context` 和 `code_with_context_start` 参数：

```json
{
  "prompt": "基于这些文件修改代码并运行测试",
  "files": ["src/App.tsx", "src/styles.css"],
  "workdir": "/Users/zoe/Developer/project"
}
```

`code_status` 参数：

```json
{
  "job_id": "返回的 job_id",
  "recent_chars": 8000
}
```

`code_result` 和 `code_cancel` 参数：

```json
{
  "job_id": "返回的 job_id"
}
```

`code_wait` 参数：

```json
{
  "job_id": "返回的 job_id",
  "timeout_seconds": 90,
  "recent_chars": 8000
}
```

`code_batch_wait` 参数：

```json
{
  "job_ids": ["job_id_1", "job_id_2"],
  "timeout_seconds": 90,
  "recent_chars": 4000
}
```

`code_batch_result` 参数：

```json
{
  "job_ids": ["job_id_1", "job_id_2"],
  "recent_chars": 4000
}
```

`code_batch_poll` 参数：

```json
{
  "job_ids": ["job_id_1", "job_id_2"],
  "seen_job_ids": [],
  "timeout_seconds": 3,
  "recent_chars": 4000,
  "include_running": false
}
```

`code_wait` 和 `code_batch_wait` 是阻塞等待工具。它们内部使用任务完成通知，不是固定间隔轮询；任务完成会立刻返回，超时才返回当前状态。

`code_batch_poll` 用于批量任务的增量读取：每次只返回还没处理过的完成、失败、取消或找不到的任务。Codex 需要把返回的 `next_seen_job_ids` 保存下来，下次作为 `seen_job_ids` 继续传入。

`code_batch_poll` 一次最多接收 500 个 `job_id`，适合几百个并发任务分批读取。

## 在 Codex 里怎么调用 Agent

最稳定的写法是直接点名 `claude-mcp`，明确工作目录，并要求 Codex 使用等待型工具。下面这些提示词可以直接复制到新 Codex 线程里测试。

### 快速连通测试

```text
请只使用 claude-mcp，不要使用内置 shell。

调用 claude-mcp 的 code 工具。

workdir: /tmp

任务：
用三句话解释 MCP Streamable HTTP 是什么。
```

### 推荐的长任务写法

```text
请只使用 claude-mcp，不要使用内置 shell。

调用 claude-mcp 的 code_start 启动任务，然后调用 code_wait 等待结果。

workdir: /Users/zoe/Developer/ai/cclaude-mcp
timeout_seconds: 120

任务：
1. 阅读当前项目结构。
2. 找出 MCP 工具实现的位置。
3. 总结每个工具的用途、参数和适合的使用场景。
4. 只返回总结，不要修改文件。
```

### 带上下文文件

```text
请只使用 claude-mcp，不要使用内置 shell。

调用 claude-mcp 的 code_with_context 工具。

workdir: /Users/zoe/Developer/ai/cclaude-mcp
files: ["README.md", "package.json"]

任务：
根据这两个文件，总结这个项目的启动方式、构建方式和发布方式。
```

### 代码审查

```text
请只使用 claude-mcp，不要使用内置 shell。

调用 claude-mcp 的 code_start 启动任务，然后调用 code_wait 等待结果。

workdir: /Users/zoe/Developer/ai/cclaude-mcp
timeout_seconds: 180

任务：
请做一次代码审查，只列出真实风险：
1. 优先找会导致运行失败、数据错误、并发问题或测试缺口的点。
2. 每个问题都要给出文件路径和原因。
3. 如果没有发现问题，明确说没有发现阻塞级问题。
4. 不要修改文件。
```

### 读写文件和命令执行测试

```text
请只使用 claude-mcp，不要使用内置 shell。

调用 claude-mcp 的 code_start 启动任务，然后调用 code_wait 等待结果。

workdir: /tmp/claude-mcp-smoke-test
timeout_seconds: 120

任务：
1. 创建一个最小 Node.js 项目。
2. 写一个 add(a, b) 函数。
3. 写一个测试文件验证 add(2, 3) 等于 5。
4. 运行测试。
5. 返回创建的文件列表、测试命令和测试结果。
```

### 并发批量任务

```text
请只使用 claude-mcp，不要使用内置 shell。

请用 claude-mcp 的 code_start 连续启动 5 个任务，每个任务都使用 workdir=/tmp。

任务 1：等待 10 秒后返回 JSON {"task":1,"ok":true}
任务 2：等待 10 秒后返回 JSON {"task":2,"ok":true}
任务 3：等待 10 秒后返回 JSON {"task":3,"ok":true}
任务 4：等待 10 秒后返回 JSON {"task":4,"ok":true}
任务 5：等待 10 秒后返回 JSON {"task":5,"ok":true}

维护 seen_job_ids = []。
全部启动后，每 3 秒调用一次 code_batch_poll：
{
  "job_ids": ["上面 5 个 job_id"],
  "seen_job_ids": seen_job_ids,
  "timeout_seconds": 3,
  "recent_chars": 4000
}
每次处理 completed / failed / cancelled / not_found。
把返回的 next_seen_job_ids 保存为新的 seen_job_ids。
当 complete=true 时停止读取并输出最终汇总。
```

### 大任务分工

```text
请只使用 claude-mcp，不要使用内置 shell。

目标项目：
workdir: /Users/zoe/Developer/ai/cclaude-mcp

请把下面工作拆成 3 个 claude-mcp 任务并发执行，每个任务用 code_start 启动：
1. 阅读 README.md，找出使用教程是否清楚。
2. 阅读 src-tauri/src/mcp.rs，整理 MCP 工具列表和参数。
3. 阅读 src-tauri/src/jobs.rs，说明异步任务和等待机制。

三个任务都不要修改文件。
全部启动后，用 code_batch_poll 每 3 秒增量读取一次。
每次处理新完成的任务，并把 next_seen_job_ids 保存为下一次的 seen_job_ids。
当 complete=true 时停止读取，合并三个结果，输出一份简洁总结。
```

## Claude Agent 能使用的本地能力

Claude MCP 的后端会把 Claude 的工具请求转成本机操作：

| 本地工具 | 能力 |
| --- | --- |
| `read_file` | 读取 UTF-8 文件 |
| `write_file` | 写入 UTF-8 文件，必要时创建父目录 |
| `list_dir` | 列出目录内容 |
| `run_command` | 在 `workdir` 下执行命令 |

命令执行规则：

- macOS 和 Linux 使用 `sh -lc`。
- Windows 使用 `cmd /C`。
- 默认超时 60 秒，最大 600 秒。
- 命令输出会截断，避免日志和返回结果过大。
- 服务只绑定 `127.0.0.1`，不会监听公网地址。

注意：只把可信目录作为 `workdir`。Claude 可以在该目录下读写文件并执行命令。

## 日志

“运行日志”页显示当前进程内的请求记录，包括：

- MCP 请求。
- new-api 请求。
- 流式返回事件。
- 本地工具调用。
- 文件读写。
- 命令执行。
- 错误和取消。

日志只存在内存里，不会写入文件。清空日志不会影响正在运行的任务。API Key 会脱敏显示。

## 用量统计

“用量统计”页会持久保存每日 token 记录，包含：

- input tokens
- output tokens
- cache read tokens
- cache write tokens
- total tokens
- request count

统计来源是上游 API 返回的 `usage` 字段。Claude MCP 会发送 `cache_control` 标记来请求缓存；如果上游没有返回 `cache_read_input_tokens`、`cache_creation_input_tokens` 或 `cache_creation` 字段，页面里的缓存读写会显示为 0。

## API 地址规则

下面两种写法都可以：

```text
https://api.example.com
https://api.example.com/v1/messages
```

最终请求都会发到：

```text
https://api.example.com/v1/messages
```

请求头包含：

```text
Authorization: Bearer <API_KEY>
anthropic-version: 2023-06-01
```

## 本地开发

环境要求：

- Node.js
- Rust
- 系统对应的 Tauri 2 依赖

安装依赖：

```bash
npm install
```

启动开发版：

```bash
npm run dev
```

只构建前端：

```bash
npm run build
```

构建当前系统安装包：

```bash
npm run tauri -- build
```

运行测试：

```bash
npm test
cd src-tauri
cargo test
```

## 发布

推送 `v*` tag 会触发 GitHub Actions，并上传 macOS、Windows、Linux 的 x64 和 ARM64 安装包。

```bash
git tag v0.1.2
git push origin main v0.1.2
```

发布工作流文件：

```text
.github/workflows/release.yml
```

## 常见问题

### Codex 看不到 claude-mcp

确认 Claude MCP 主控台显示“运行中”，再执行：

```bash
codex mcp list
codex mcp get claude-mcp
```

如果刚添加完 MCP，重新打开一个 Codex 线程。

### 端口被占用

默认端口是 `8765`。如果启动失败，在主控台换一个 1024 以上的端口，然后重新执行：

```bash
codex mcp remove claude-mcp
codex mcp add claude-mcp --url http://127.0.0.1:8787/mcp
```

上面的 `8787` 换成你在主控台填写的新端口。

### macOS 提示无法打开

当前安装包没有 Apple Developer ID 公证。可以右键打开，或执行：

```bash
xattr -dr com.apple.quarantine "/Applications/Claude MCP.app"
```

### 后台看不到缓存读写

缓存读写取决于上游是否支持并返回 Claude 兼容的缓存字段。Claude MCP 会发送缓存标记；如果上游没有返回缓存字段，本地统计和后台都可能显示为 0。
