# cursor-byok · Cursor 3.27.22 适配版

基于 [leookun/cursor-byok](https://github.com/leookun/cursor-byok) 的个人维护分支，主要维护 Cursor 新版协议适配和 Windows x64 桌面包。

在 Cursor 中连接自己配置的模型服务，支持 **OpenAI Chat Completions、OpenAI Responses 和 Claude / Anthropic Messages** 三种接口。接口按服务商实际支持的协议选择，不按模型名称区分。

[下载 Windows 版](https://github.com/curocu/cursor-byok/releases/tag/windows-v1.0.6) · [配置说明](./README-CN.md) · [English](./README-EN.md) · [提交问题](https://github.com/curocu/cursor-byok/issues) · [上游仓库](https://github.com/leookun/cursor-byok)

## 下载

当前版本 **1.0.6**，已在 Windows x64 和 Cursor **3.27.22** 环境构建、测试。

- [免安装 EXE](https://github.com/curocu/cursor-byok/releases/download/windows-v1.0.6/Cursor-BYOK-1.0.6-portable.exe)：直接运行，适合先体验页面或手动替换旧版。
- [Windows x64 安装包](https://github.com/curocu/cursor-byok/releases/download/windows-v1.0.6/Cursor-BYOK-1.0.6-x64-setup.exe)：按安装向导安装，可选择 D 盘目录。
- [SHA-256 校验文件](https://github.com/curocu/cursor-byok/releases/download/windows-v1.0.6/SHA256SUMS-1.0.6.txt)。

运行发布包不需要安装 Rust、Node.js 或 CMake。当前只提供 Windows x64 安装包，未发布本分支的 macOS / Linux 构建。

**升级方式：**先退出旧 BYOK，再运行新版，避免多个实例同时接管 Cursor。当前包的应用内更新地址仍继承上游；本分支请从这里手动下载安装，避免上游更新覆盖本分支的适配。

## 本分支改了什么

- **Cursor 3.27.22 协议适配**：接入新版 `RunInferenceSSE` 和 `BidiAppend`，修复升级后出现的 `protobuf decode error` / `delimited length exceeded`。
- **三种接口统一接入**：处理工具 schema、JSON 数字类型、工具结果回传，以及各接口的思考状态和签名。
- **Agent 能力补齐**：补齐模式配置、子 Agent 模型选择、流式回复、取消和心跳处理。
- **本地模型后台记录处理**：本地模型的部分分类、标注请求在本地应答，避免把本地模型 ID 发给官方接口。
- **1.0.6 显示调整**：不再回传路由模型显示名称，隐藏新聊天中的 “Routed to …” 提示及其对应评价按钮；实际模型路由不变。
- **Windows 交付**：提供免安装 EXE、NSIS 安装包和把缓存、临时文件及构建产物放在 D 盘的构建脚本。

详细适配范围见 [Windows 适配与构建说明](./docs/cursor-3.27.22-windows.md)，本次发布说明见 [1.0.6](./docs/releases/1.0.6.md)。

## 快速开始

1. 下载并启动一个 BYOK 实例，在 Cursor 配置页按提示完成本地证书和接管配置。
2. 在模型设置中添加服务地址、API Key、上游模型 ID，并选择服务商实际支持的接口。
3. 保存后运行连接测试，确认流式回复和模型服务可用。
4. 重新打开 Cursor，新建聊天，手动选择自己的模型配置后发送消息。
5. 新增或修改模型后，可在 Cursor 模型设置页点击刷新；仍未显示时，执行 `Developer: Reload Window`，再重新选择模型。

API Key 和配置保存在本机；聊天内容会发送到你配置的模型服务商。模型服务的费用和可用性由对应服务商决定。

## 已验证范围

- 前一版累计通过 184 项 Rust 单元与协议测试；1.0.6 的 14 项相关协议、连接生命周期和三种接口契约回归测试全部通过。
- 1.0.6 前端类型检查、生产构建、Windows EXE 和 NSIS 打包通过。
- 此前已在真实 Cursor 中验证普通 Agent、文件工具循环和 Task 工具拉起的子 Agent，并验证真实 Chat Completions / Responses 通道。
- Claude 接口的本地契约测试通过；当时真实上游通道不可用，尚未完成真实 Claude 回复验收。
- 独立 Multitask 模式完整流程、所有第三方模型、所有 Cursor 版本均未承诺兼容。1.0.6 隐藏提示的 Cursor 界面效果仍需在新版的新聊天中验收。

模型列表的同名通道展示、修改配置后关闭状态保留等问题尚未包含在 1.0.6 中。

## 从源码构建

Windows 构建需要 Rust MSVC 工具链、Visual Studio C++ Build Tools、Windows SDK、Node.js / npm 和 CMake；桌面运行依赖 WebView2。

在仓库根目录执行，CMake 路径按本机安装位置调整：

```powershell
powershell -ExecutionPolicy Bypass -File .\scripts\build-windows.ps1 -CMakeBin 'D:\devtools\cmake-3.31.12-windows-x86_64\bin'
```

默认构建目录是 `D:\devtools\cursor-byok`，可通过 `-BuildRoot` 指定。签名自动更新尚未配置，本次发布使用本机构建产物。仓库的跨平台 CI 尚未全部通过，不代表已有可用的跨平台发布包。

## 上游与许可

原项目及主要功能来自 [leookun/cursor-byok](https://github.com/leookun/cursor-byok)，通用配置可参考[上游文档](https://docs.leokun.cn)。本分支仅维护这里列出的适配与交付改动，与 Cursor 官方无关联。

沿用 [MIT 许可证](./LICENSE)，保留原作者的版权和许可声明。欢迎向上游项目提供支持。
