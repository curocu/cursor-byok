# cursor-byok · Cursor 3.27.22 adaptation

A personal maintenance branch of [leookun/cursor-byok](https://github.com/leookun/cursor-byok), focused on newer Cursor protocol support and Windows x64 desktop builds.

Connect Cursor to model services you configure through OpenAI Chat Completions, OpenAI Responses, or Claude / Anthropic Messages. Select the API protocol supported by your provider, independently of the model's brand or display name.

[中文](./README.md) · [Windows downloads](https://github.com/curocu/cursor-byok/releases/tag/windows-v1.0.6) · [Report an issue](https://github.com/curocu/cursor-byok/issues) · [Upstream](https://github.com/leookun/cursor-byok)

## Download

Version **1.0.6**, built and tested on Windows x64 with Cursor **3.27.22**:

- [Portable EXE](https://github.com/curocu/cursor-byok/releases/download/windows-v1.0.6/Cursor-BYOK-1.0.6-portable.exe)
- [Windows x64 installer](https://github.com/curocu/cursor-byok/releases/download/windows-v1.0.6/Cursor-BYOK-1.0.6-x64-setup.exe)
- [SHA-256 checksums](https://github.com/curocu/cursor-byok/releases/download/windows-v1.0.6/SHA256SUMS-1.0.6.txt)

The packaged app does not require Rust, Node.js, or CMake. This branch currently publishes Windows x64 artifacts only.

Exit the previous BYOK instance before starting the new one. The app's built-in update endpoints still point to upstream; update this branch manually from this repository to keep these adaptations.

## Changes in this branch

- Adapt the newer `RunInferenceSSE` / `BidiAppend` protocol used by Cursor 3.27.22, addressing `protobuf decode error` / `delimited length exceeded` after upgrading.
- Handle tool schemas, JSON number types, tool result submission, and provider-specific reasoning state across the three supported API formats.
- Supply mode and subagent metadata, streaming responses, cancellation, and heartbeat handling.
- Respond locally to selected classification and labeling requests for local models.
- In 1.0.6, omit the routed model display name to hide the “Routed to …” label and its rating controls in new chats, without changing the actual model route.
- Provide Windows portable and NSIS packages, with a build script that keeps build artifacts, temporary files, and dependency caches on D: by default.

See the [Windows adaptation notes](./docs/cursor-3.27.22-windows.md) and [1.0.6 release notes](./docs/releases/1.0.6.md) for details.

## Quick start

1. Download and run one BYOK instance. Follow its Cursor configuration page to initialize the local certificate and integration.
2. Add your provider URL, API key, model ID, and the API format your provider supports.
3. Save the configuration and run its connection test.
4. Reopen Cursor, create a new chat, and select the configured model explicitly.
5. After changing models, refresh Cursor's model settings. If needed, run `Developer: Reload Window` and select the model again.

Keys and configuration are stored locally. Model requests are sent to your chosen provider, whose pricing and availability apply.

## Validation and limits

The preceding version passed 184 Rust unit and protocol tests. For 1.0.6, all 14 relevant protocol, lifecycle, and provider contract regression tests passed, as did frontend type checks and the Windows production build and packaging.

Earlier real Cursor testing covered ordinary Agent conversations, file tool loops, and subagents launched with Task. Real Chat Completions and Responses routes were also exercised. Anthropic contract tests passed, but the configured live Claude channel was unavailable. The complete standalone Multitask workflow and the new label's appearance in a live 1.0.6 chat still require acceptance testing.

Duplicate names across channels and preserving disabled state after configuration identity changes are not fixed in this release. Cross-platform CI has not fully passed; no macOS or Linux artifacts are provided by this release.

## Build on Windows

Install Rust with the MSVC toolchain, Visual Studio C++ Build Tools, the Windows SDK, Node.js / npm, and CMake. The desktop app uses WebView2.

Run from the repository root, adjusting the CMake directory as needed:

```powershell
powershell -ExecutionPolicy Bypass -File .\scripts\build-windows.ps1 -CMakeBin 'D:\devtools\cmake-3.31.12-windows-x86_64\bin'
```

Use `-BuildRoot` to override the default `D:\devtools\cursor-byok` directory. Signed automatic updates are not configured; this release contains locally built artifacts.

## Upstream and license

The original project and most functionality come from [leookun/cursor-byok](https://github.com/leookun/cursor-byok). General setup information is available in the [upstream documentation](https://docs.leokun.cn).

This independent branch is not affiliated with Cursor. It retains the original copyright notice and [MIT license](./LICENSE). Please support the upstream project.
