# Cursor BYOK 配置说明

[项目首页](./README.md) · [Windows 下载](https://github.com/curocu/cursor-byok/releases/tag/windows-v1.0.24) · [English](./README-EN.md)

从旧版升级可查看 [1.0.24 完整更新记录](./docs/releases/1.0.24.md)。

## 聊天模型

在 BYOK 的 **Cursor** 页面添加模型。按照服务商提供的 API 选择协议：

- **OpenAI Chat Completions** 使用 `/v1/chat/completions`。
- **OpenAI Responses** 使用 `/v1/responses`。
- **Claude / Anthropic Messages** 使用 `/v1/messages`。

填写服务基础地址，或启用完整请求地址后填写完整 URL。填入服务商提供的 API Key 和模型 ID。显示名称用于 Cursor 菜单，分组可以帮助区分同名通道。上下文窗口、输出上限和思考参数按服务商支持范围配置。

保存并运行连接测试后，在 Cursor 中选择该模型。Cursor 在下次读取模型目录时收到更新。需要立即刷新时，打开 **Cursor Settings → Models → Refresh model list**。

## 图片生成

打开 **系统设置 → 图片生成**，选择 **管理模型**，添加 Images API 或 Responses 生图模型。选择一个默认生图模型后，Cursor Agent 可通过原生生图工具调用它。生图模型单独设置。

在 **生图参数** 中可配置尺寸、画质、格式、背景、内容审核、生成复杂度和压缩率。未指定时采用模型配置或让上游决定。压缩率用于 JPEG 和 WebP，透明背景使用 PNG 或 WebP。扩展尺寸、自定义分辨率和部分选项依赖上游支持。

点击 **生图测试** 后输入描述，可临时选择模型和参数。生成成功后点击 **查看图片** 打开预览。

## 问题反馈

请提供 Cursor 与 BYOK 版本、上游协议、可复现步骤、发生时间和脱敏后的错误信息。有 Request ID 时可一并提供。请勿贴出 API Key、完整请求头、私人聊天或数据库。 [提交问题](https://github.com/curocu/cursor-byok/issues)

## 从源码构建

Windows 构建需要 Rust MSVC 工具链、Visual Studio C++ Build Tools、Windows SDK、Node.js / npm 和 CMake。桌面应用使用 WebView2。在仓库根目录执行以下命令，按实际安装位置调整 CMake 路径。

```powershell
.\scripts\build-windows.ps1 -CMakeBin 'D:\devtools\cmake-3.31.12-windows-x86_64\bin'
```

脚本默认将缓存和构建产物写到 `D:\devtools\cursor-byok`，可通过 `-BuildRoot` 指定其他目录。已安装此分支的用户可从 GitHub Releases 手动升级，应用内更新仍指向上游。
