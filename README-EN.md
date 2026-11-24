# Cursor BYOK

**Supports the current Cursor 3.31 stable series, verified on Cursor 3.31.15 for Windows x64.** [Official Cursor downloads](https://cursor.com/download)

Connect Cursor to your own model services while keeping its native chat, Plan mode, subagents, and tool calls. Supports OpenAI Chat Completions, OpenAI Responses, and Claude Messages.

[Download Windows x64 1.0.24](https://github.com/curocu/cursor-byok/releases/tag/windows-v1.0.24) · [中文](./README.md) · [Setup and build instructions (Chinese)](./README-CN.md)

## Fixes included in 1.0.24

- **Startup and desktop:** Fixed startup on previously upgraded databases, lost theme preferences, and pages resetting when reopening the window.
- **Agent and connection:** Fixed reconnect loops, Task subagent startup, missing Plan transitions, and stopping or resuming tasks with child agents.
- **Models and context:** Fixed official models reappearing after refresh, model settings being overridden in requests, and context usage display. Enabled Cursor's native automatic summarization.
- **Output and errors:** Fixed missing or duplicate text, content ordering, and tool error flags. Partial output survives token limits, and upstream errors include retry details.
- **Image generation:** Added native Cursor image generation and fixed missing tools, options moving between protocols, ambiguous model names, and scrolling on small windows.

[Complete release notes (Chinese)](./docs/releases/1.0.24.md)

Cursor 3.31.15 protocol compatibility and existing live Responses calls were checked. Local tests cover all three chat protocols. The latest image generation and saving flow still needs live Cursor verification.

This fork is based on leookun/cursor-byok and uses the [MIT license](./LICENSE).
