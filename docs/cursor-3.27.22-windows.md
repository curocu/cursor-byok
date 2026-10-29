# Cursor 3.27.22 Windows 适配与构建

适配版本：Cursor 3.27.22；桌面包版本：1.0.6。

1.0.6 不再回传路由模型显示名称，隐藏 Cursor 的 “Routed to …” 提示及其评价按钮。
实际模型路由、工具调用与流式回复保持不变；已存在的聊天记录可能仍保留原提示。

## 修复范围

Cursor 的本地 Agent 循环通过 `InferenceService/RunInferenceSSE` 和共享的
`BidiService/BidiAppend` 发起调用。旧服务按 `AgentClientMessage` 解码新消息，
造成 `AgentRunRequest.conversation_state … delimited length exceeded`。

本次新增协议转换，并接入现有三种 Provider：

- OpenAI Chat Completions：包括采用该接口的第三方模型。
- OpenAI Responses：包括工具调用后的加密推理状态回传。
- Claude / Anthropic Messages：包括思考签名回传。

接口选择取自模型配置，与模型显示名称无关。统一处理工具 schema 的
`jsonSchema` 包装、Protobuf 数字恢复为 JSON 整数、流式调用 ID 和取消。
握手补齐 Agent、Ask、Plan、Debug、Triage、Multitask 的模式配置，
以及配置中的子 Agent 模型与覆盖策略。

1.0.5 另外补齐可选后台记录：本地模型的 follow-up 分类和 post-turn 标注
在本地返回空应答，不执行官方分类算法，也不把本地模型 hash 发给官方服务；
官方模型仍转发原请求。握手明确声明 `all-static` 工具布局，并接收 Agent 事件。

## 已验证范围

- Rust 单元与协议测试：181 项通过。
- 三种 Provider 的本地 HTTP 契约测试：3 项通过；包含工具循环与推理状态回传。
- 真实代理链路：Chat Completions 与 Responses 各携带 20 个 Cursor 工具，
  完成工具调用、工具结果回传、第二轮回复。
- Cursor 界面的普通 Agent、工具循环已确认实际调用 BYOK；Task 工具拉起的
  3 个子 Agent 也已实际调用 BYOK。独立 Multitask 模式完整流程仍需验收。
- 当前 Claude 配置的真实上游返回 503 `model_not_found` / 无可用通道；
  因此 Claude 的真实模型回复尚未验证。

当前适配 HTTP/1 SSE。未实现原生双向 `RunInference`、`RunInferencePoll`、
增量或压缩请求；服务不协商后两项。直接文件内容及 Provider 自带工具尚未支持。
后台分类/标注 RPC 已对本地模型作本地应答。真实使用中另观察到一次
Responses 上游流在 `response.completed` 前中断，客户端如实显示错误。
本机直连 Google 也超时，部分联网工具仍依赖可用的网络出口。
不能据此宣称任意模型与所有 Cursor 功能已经验证。

## 本机环境

Windows x64、Rust MSVC、Node.js/npm、Visual Studio 2022 C++ Build Tools、
Windows SDK、CMake、WebView2 已具备，已成功构建 EXE 与 NSIS 安装包。
运行打包结果不需要另外安装 Rust、Node.js 或 CMake。

CMake：`D:\devtools\cmake-3.31.12-windows-x86_64\bin`。
新增依赖缓存、临时目录、构建工具与产物均位于 `D:\devtools\cursor-byok`。

从仓库根目录执行：

```powershell
powershell -ExecutionPolicy Bypass -File .\scripts\build-windows.ps1
```

脚本默认将 Cargo、npm 缓存、TEMP/TMP、target 放在 D 盘。
`tauri.local.conf.json` 使用本地工具目录，仅构建 NSIS，关闭更新签名产物。
没有配置发布签名，不生成 GitHub Release。

输出：

```text
D:\devtools\cursor-byok\target\release\cursor-byok-desktop.exe
D:\devtools\cursor-byok\target\release\bundle\nsis\Cursor BYOK_1.0.6_x64-setup.exe
```

## 手工验收

1. 退出旧 BYOK，再运行新版；测试后端运行期间不要同时启动第二个实例。
2. 打开 Cursor，新建聊天，在 BYOK 中按真实服务选择对应接口。
3. 普通 Agent 测试一句回复，然后让它读取测试文件、修改测试文件，再追问内容。
4. 在多 Agent 模式重复测试，确认子 Agent 能完成并返回结果。
5. 在长回复中点停止，确认调用被取消。

应用接管代理设置时可能关闭 Cursor，之后重新打开即可。
本地构建未作代码签名；安装包可选择 D 盘路径。

## 协议更新

`support/cursor-protocol-extractor/extract_inference.py` 只把本机 Cursor
Agent harness 文件作为文本读取，不执行其中代码。可重新提取：

```powershell
python .\support\cursor-protocol-extractor\extract_inference.py <agent-harness.cjs路径> .\protocols\cursor\inference_v1.proto
```

不要提交本机 harness、API 密钥、数据库、调用正文或诊断日志。
