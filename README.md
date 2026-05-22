# Claude MCP

Claude MCP is a cross-platform Tauri 2 desktop app that exposes a local MCP Streamable HTTP server and forwards Claude Code-style tool calls directly to a Claude-compatible `/v1/messages` API.

The app does not require the Claude Code CLI. It provides a desktop control panel for API configuration, MCP server startup, live logs, and token usage statistics.

## Features

- Local MCP endpoint at `http://127.0.0.1:8765/mcp`
- Direct Claude-compatible Messages API calls
- Tools compatible with the Claude Code MCP shape:
  - `code`
  - `code_with_context`
  - `code_start`
  - `code_with_context_start`
  - `code_status`
  - `code_result`
  - `code_cancel`
  - `code_async`
- In-memory request logs
- Persistent daily token usage statistics
- Local API key storage in the app config file
- Prompt cache markers for Claude-compatible providers

## Development

```bash
npm install
npm run dev
```

## Build

```bash
npm run build
npm run tauri -- build
```

## Release

Pushing a tag like `v0.1.0` runs the release workflow and uploads installers for macOS, Windows, and Linux on x64 and ARM64 where the GitHub runner supports that platform.

```bash
git tag v0.1.0
git push origin main v0.1.0
```
