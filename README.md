# Cursor BYOK

**适配当前 Cursor 3.31 稳定版系列，已在 Windows x64 的 Cursor 3.31.15 上验证。** [Cursor 官方下载](https://cursor.com/download)

把自己的模型服务接入 Cursor，继续使用原生聊天、计划模式、子 Agent 和工具调用。支持 OpenAI Chat Completions、OpenAI Responses 和 Claude Messages。

[下载 Windows x64 1.0.24](https://github.com/curocu/cursor-byok/releases/tag/windows-v1.0.24) · [配置说明](./README-CN.md) · [English](./README-EN.md)

## 1.0.24 累计修复

- **启动与桌面**：修复升级过的数据库无法启动、主题丢失和关窗重开后页面重置。
- **Agent 与连接**：修复发消息后反复重连、Task 子 Agent 启动失败、计划模式切换缺失，以及停止后子任务继续执行或聊天无法恢复。
- **模型与上下文**：修复刷新后官方模型混入、模型参数与实际请求不一致、上下文用量显示异常，并接通 Cursor 原生自动摘要。
- **回复与错误**：修复分段文字丢失或重复、工具结果顺序和失败标记异常。达到输出上限时保留已生成内容，并显示上游错误与重试提示。
- **图片生成**：接入 Cursor 原生生图，修复工具缺失、切换协议后参数错位、同名模型难区分，以及小窗口下测试弹窗无法滚动。

[完整更新记录](./docs/releases/1.0.24.md)

已核对 Cursor 3.31.15 的内置 Agent 协议，并从真实记录确认 Responses 聊天和工具结果回传。三种聊天协议均有本地测试覆盖，本版在真实 Cursor 中的生图和保存仍待验收。

本项目基于 leookun/cursor-byok，遵循 [MIT 许可证](./LICENSE)。
