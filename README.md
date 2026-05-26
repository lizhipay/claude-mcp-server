# Claude MCP

Claude MCP 是一个跨平台桌面软件，用 Tauri 2、Rust、React 和 TypeScript 构建。它在本机启动一个 MCP Streamable HTTP 服务，让 Codex 等 MCP 客户端可以在不安装 Claude Code CLI 的情况下，把任务交给 Claude Agent SDK 执行。

默认 MCP 地址是：

```text
http://127.0.0.1:8765/mcp
```

## 适合做什么

- 让 Codex 调用一个本地 Claude Agent。
- 使用 Claude Code 同源的 Agent 能力读写文件、搜索项目、修改代码和执行命令。
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

## Agent 内核

默认内核是官方 `@anthropic-ai/claude-agent-sdk`。它使用 Claude Code 同源的 agent loop、工具、上下文管理、session、hooks 和权限系统；安装包会携带 Agent SDK native binary，不需要用户单独安装 Claude Code CLI。

Claude MCP 仍保留 `legacy` 内核作为兜底。如果 Agent SDK bridge 在当前机器上无法启动，任务会自动切回 legacy，并在运行日志里写清楚原因。普通用户不需要手动切换。

开发环境需要先执行：

```bash
npm install
```

然后使用：

```bash
npm run dev
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

## 在 Codex 里怎么用

这个软件最适合的用法是：Codex 做调度和验收，Claude MCP 负责真正写代码、改文件、跑命令。你可以把它当成一个本机 Claude Code Agent，Codex 只需要把任务交给 `claude-mcp`。

推荐流程：

1. 打开 Claude MCP，保存 API 配置，点击“启动服务”。
2. 在 Codex 里确认已经添加 `claude-mcp`。
3. 新开一个 Codex 线程，把下面的提示词复制进去。
4. Codex 调用 Claude MCP 写代码。
5. Codex 读取结果、检查文件、跑必要验证。

### 给 Codex 的固定开场白

每次想让 Codex 调用这个软件时，可以先把这一段贴进去：

```text
请优先使用 claude-mcp 来完成任务。

要求：
1. 需要写代码、改文件、跑命令的部分，交给 claude-mcp 执行。
2. 你负责监督进度、读取结果、检查产物和汇报结论。
3. 明确传入 workdir，不要让 claude-mcp 在错误目录里工作。
4. 长任务使用 code_start，然后用 code_wait 或 code_batch_poll 读取结果。
5. 如果工具列表里暂时看不到 mcp__claude_mcp__，先提醒我检查 Claude MCP 是否启动，不要假装已经执行。
```

### 最小可用测试

第一次连接时，用这个测试最简单：

```text
请使用 claude-mcp 的 code 工具执行一个连通测试。

workdir: /tmp

任务：
返回一句话：Claude MCP 已经可以被 Codex 调用了。
```

期望结果：Codex 会调用 `claude-mcp`，然后返回 Claude MCP 的回答。如果 Codex 说工具不存在，通常是 Claude MCP 没启动、Codex 没重新打开线程，或 MCP 名字没有添加成功。

### 让 Claude MCP 修改一个项目

适合修 bug、加功能、改 README、做小页面：

```text
请使用 claude-mcp 完成下面的代码任务。

workdir: /Users/zoe/Developer/your-project

任务：
1. 阅读项目结构。
2. 找到用户登录页面。
3. 修复登录失败时错误提示不明显的问题。
4. 保持现有 UI 风格，不要重构无关代码。
5. 修改完成后运行项目已有测试或最接近的检查命令。
6. 最终返回：修改了哪些文件、验证命令、验证结果。

执行方式：
- 用 code_start 启动任务。
- 用 code_wait 等待结果，timeout_seconds 设置为 300。
- 不要只给计划，必须让 claude-mcp 实际改文件。
```

### 让 Claude MCP 写一个完整页面

适合把一个需求直接交给 Claude MCP 做完：

```text
请使用 claude-mcp 写一个完整页面。

workdir: /Users/zoe/Developer/your-project

任务：
做一个订单列表页面，要求：
1. 能展示订单号、客户、金额、状态、创建时间。
2. 支持按状态筛选。
3. 支持搜索订单号和客户名。
4. 保持项目现有组件和样式习惯。
5. 如果需要新增文件，可以新增。
6. 完成后运行类型检查或构建命令。
7. 最终返回可访问路径、修改文件、验证结果。

执行方式：
- 使用 code_start。
- 如果超过 5 分钟还没完成，用 code_status 看最近输出。
- 完成后用 code_result 读取最终结果。
```

### 让 Claude MCP 做代码审查

适合让 Claude MCP 只读检查，不改文件：

```text
请使用 claude-mcp 做一次代码审查。

workdir: /Users/zoe/Developer/your-project

任务：
1. 只读检查，不要修改文件。
2. 优先找会导致运行失败、数据错误、并发问题、安全问题或测试缺口的点。
3. 每个问题都要写清楚文件路径、原因和建议修复方式。
4. 如果没有发现明显问题，明确说没有发现阻塞级问题。

执行方式：
- 使用 code_start。
- 使用 code_wait 等待结果。
```

### 让 Claude MCP 自己启动服务

适合做小游戏、静态页面、Demo 项目：

```text
请使用 claude-mcp 在下面目录里完成一个可运行 Demo。

workdir: /Users/zoe/Developer/ai/test

任务：
1. 做一个完整可玩的贪吃蛇游戏。
2. 二次元可爱风格。
3. 支持键盘方向键、WASD 和移动端触控。
4. 包含开始、暂停、重新开始、得分、最高分。
5. 写完后自己启动本地服务。
6. 服务只监听 127.0.0.1，端口任选可用端口。
7. 启动后用 curl 验证首页能访问。
8. 最终返回浏览器可以直接打开的 URL。

执行方式：
- 使用 code_start。
- 用 code_wait 等待结果。
- 如果服务被前台进程挂住，读取到 URL 后再让 claude-mcp 单独关闭服务。
```

### 多个任务并发处理

适合让 Claude MCP 同时做多件互不影响的事。Codex 负责启动任务和收结果：

```text
请使用 claude-mcp 并发完成下面 3 个只读任务。

workdir: /Users/zoe/Developer/your-project

任务 A：
阅读 README.md，判断新用户能不能顺利启动项目。

任务 B：
阅读 package.json 和 src 目录，整理项目的构建、测试和开发命令。

任务 C：
阅读最近改动，找出可能缺少测试的地方。

执行方式：
1. 用 code_start 分别启动 3 个任务。
2. 保存返回的 job_id。
3. 维护 seen_job_ids = []。
4. 每 3 秒调用一次 code_batch_poll：
   {
     "job_ids": ["任务 A 的 job_id", "任务 B 的 job_id", "任务 C 的 job_id"],
     "seen_job_ids": seen_job_ids,
     "timeout_seconds": 3,
     "recent_chars": 4000
   }
5. 每次处理 completed / failed / cancelled / not_found。
6. 把返回的 next_seen_job_ids 保存为新的 seen_job_ids。
7. complete=true 后停止读取，合并结果给我。
```

### 让 Codex 监督 Claude MCP 写完再验收

这是最推荐的真实工作提示词：

```text
请把自己当成监督者，把写代码的工作交给 claude-mcp。

目标目录：
workdir: /Users/zoe/Developer/your-project

需求：
修复用户反馈的这个问题：保存按钮点击后没有任何提示。

执行要求：
1. 你不要直接改代码。
2. 调用 claude-mcp 的 code_start，让它检查项目、修改代码、运行验证。
3. 用 code_wait 或 code_batch_poll 读取结果。
4. Claude MCP 完成后，你再检查 git diff，确认改动范围合理。
5. 如果验证失败，把失败原因继续交给 claude-mcp 修。
6. 最后只汇报：改了什么、验证是否通过、还有没有风险。
```

### 常见情况怎么处理

| 情况 | 推荐做法 |
| --- | --- |
| 任务很短 | 用 `code` |
| 任务可能超过 1 分钟 | 用 `code_start` + `code_wait` |
| 同时启动很多任务 | 用多个 `code_start` + `code_batch_poll` |
| 只想读结果，不等待 | 用 `code_result` 或 `code_batch_result` |
| 任务卡住 | 先用 `code_status` 看最近输出，再决定是否 `code_cancel` |
| Codex 说工具不存在 | 确认 Claude MCP 已启动，并重新打开 Codex 线程 |
| Claude 在错误目录工作 | 提示词里写清楚绝对路径 `workdir` |

## Claude Agent 能使用的本地能力

默认 SDK 内核使用 Claude Code 的内置工具：

| 能力 | 说明 |
| --- | --- |
| 文件读取 | 读取 `workdir` 内相关文件 |
| 文件编辑 | 使用 Claude Code 的编辑能力修改文件 |
| 项目搜索 | 使用 Glob/Grep 等工具查找代码 |
| 命令执行 | 在 `workdir` 下执行开发命令 |
| Todo / session | 复用 Claude Code 的任务规划和上下文管理 |

legacy 内核仍保留旧工具：

| legacy 工具 | 能力 |
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

注意：只把可信目录作为 `workdir`。SDK 内核使用 `bypassPermissions` 完全放行模式，Claude 可以直接读写文件、执行命令和调用内置工具；Claude MCP 不再额外拦截危险命令或敏感路径。

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
