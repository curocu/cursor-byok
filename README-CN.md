# Cursor BYOK 配置说明

[返回项目首页](./README.md) · [下载 Windows 1.0.6](https://github.com/curocu/cursor-byok/releases/tag/windows-v1.0.6) · [English](./README-EN.md)

## 安装和升级

下载本仓库 Release 中的免安装 EXE 或 x64 安装包。安装包可选择 D 盘目录；运行应用不需要开发工具链。

升级前退出旧 BYOK，并确保没有另一个测试后台同时接管 Cursor。启动新版，在 Cursor 配置页按提示完成本地证书和接管配置；接管过程中可能关闭 Cursor，之后重新打开即可。

当前应用内更新仍指向上游，本分支暂时通过本仓库 Release 手动升级。

## 选择接口

接口取决于服务商提供的 API，不能仅根据 GPT、DeepSeek 等模型名称判断。

- **OpenAI Chat Completions**：用于兼容 `/v1/chat/completions` 的模型服务。
- **OpenAI Responses**：用于兼容 `/v1/responses` 的模型服务。
- **Claude / Anthropic Messages**：用于兼容 `/v1/messages` 的模型服务。

同一模型可能由不同服务商提供不同接口，选择后应运行连接测试。工具调用和思考参数也必须由上游实际支持。

## 填写模型配置

- **服务地址**：填写基础地址，由 BYOK 追加标准端点；或启用完整 URL，直接使用完整请求地址。
- **API Key**：填写该服务商的访问密钥。
- **模型 ID**：填写服务商接受的模型标识，以服务商接口或模型列表为准。
- **显示名称**：用于 Cursor 模型列表显示，不改变发送给上游的模型 ID。
- **分组**：用于区分同名模型的不同通道；未设置时，列表徽章会使用服务地址的主机名。
- **上下文、输出和思考参数**：根据模型服务支持的范围填写；自定义 Headers 和额外参数使用 JSON 对象。

保存并测试后，重新打开 Cursor，新建聊天并选择该模型。`Auto` 使用官方模型，不会自动选择自定义 BYOK 通道。

## 模型列表刷新

BYOK 保存配置后立即写入本地数据库，但没有主动向 Cursor 推送列表的机制。Cursor 下一次请求模型目录时才会读取最新内容。

可先点击 Cursor 模型设置页的刷新按钮；仍未更新时，按 `Ctrl+Shift+P`，执行 `Developer: Reload Window`，然后重新选择模型。

不同通道可以使用相同显示名称，此时请结合来源徽章区分。当前修改地址、密钥、模型 ID 或显示名称可能改变内部 ID，因此原来的关闭状态可能无法沿用；1.0.6 尚未修复这一问题。

## 1.0.6 的路由提示

本版本不再向 Cursor 回传 `routed_model_display_name`，用于隐藏 “Routed to …” 及对应评价按钮。实际模型路由、工具调用和流式回复逻辑不变。

旧聊天已保存的提示可能继续保留，需在新聊天中验证。此改动不隐藏模型选择器中的模型名称或通道来源。

## 问题反馈

请在[本仓库 Issues](https://github.com/curocu/cursor-byok/issues)提供 Cursor 版本、BYOK 版本、接口类型、复现步骤和脱敏错误信息。不要提交 API Key、完整请求头或本地数据库。

已验证范围、现有限制和构建方法见[项目首页](./README.md)和 [Windows 适配说明](./docs/cursor-3.27.22-windows.md)。
