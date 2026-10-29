# Kiro 接入方案（可行性分析 + 实现方案）

调研对象：`leookun/cursor-byok`（本地快照 `D:\download\cursor-byok-main`）+ 本机已安装的 Kiro IDE。
调研日期：2026-10-14。本方案只做分析与设计，不含代码改动。

---

## 1. 结论摘要

**可行，而且比现有的 Devin 接入省事得多。**

| 判断项 | 结论 |
|---|---|
| Kiro 能否指向本机网关 | 能。Kiro 官方在扩展里读取 `codewhisperer.config` 下的三个端点覆盖设置 |
| 是否需要本地 CA / MITM | **不需要**。没有证书固定（pinning），而且根本不用碰 TLS |
| 是否需要给 Kiro 打补丁 | **不需要**。改一个 JSON 设置即可，比 Devin 的宿主补丁干净 |
| 需要实现什么 | 一个 HTTP 服务，实现 `GET /ListAvailableModels` + `POST /generateAssistantResponse`（或 `POST /v1/responses`） |
| 主要成本 | 协议仿真（AWS event-stream 二进制帧 / SSE）、工具调用映射、前端设置页 |
| 预估工作量 | MVP 8–12 人天；完整 15–20 人天（不含黑盒抓包调试的不可预期时间） |

与 Devin 模块对比：Devin 需要 11 个 Rust 文件（约 150 KB）**外加**宿主 `extension.js` 补丁、SHA-256 备份回滚、安装探测；Kiro 可以省掉整个宿主补丁链路（`host_patch.rs` / `host_detect.rs` / `model_uids.rs` 都不需要），预计 6–8 个文件、约 60–90 KB。

---

## 2. 本机环境与证据

### 2.1 安装位置

| 项目 | 值 |
|---|---|
| Kiro IDE | `D:\Program Files\Kiro\Kiro.exe`（约 203 MB） |
| 版本 | `1.3.0`，commit `12d263fccd805ef7e63ee3109df3866501796c71`（2026-10-12） |
| AI 扩展 | `kiro.kiro-agent` v`1.1.382`（`publisher: kiro`，`license: AWS-IPL`） |
| 扩展主 bundle | `D:\Program Files\Kiro\resources\app\extensions\kiro.kiro-agent\dist\extension.js`（14,501,349 字节） |
| Electron 主进程 | `D:\Program Files\Kiro\resources\app\out\main.js` |
| 官方 CLI | `D:\Program Files\Kiro\bin\kiro.cmd`（转调 `out\cli.js`） |
| 用户数据 | `%APPDATA%\Kiro`（含 `User\settings.json`、`logs`）、`%USERPROFILE%\.kiro` |
| 登录凭证 | `%USERPROFILE%\.aws\sso\cache\kiro-auth-token.json` |

> 注意：`D:\download\kiro-client.exe` **不是**官方 Kiro CLI，是第三方账号池/注册工具（基于 WebView2），与本方案无关。

### 2.2 端点解析（已在 bundle 中逐字核实）

`extension.js` 中端点由这段逻辑决定（原文）：

```js
function eei(t,e){let r=qeu(e)??"us-east-1";return{region:r,endpoint:e.endpointOverride||`https://${t}.${r}.kiro.dev`}}
function b$(t){return eei("management",t)}   // 控制面
function n8(t){return eei("runtime",t)}      // 模型推理
```

即默认端点是 `https://runtime.<region>.kiro.dev`（推理）和 `https://management.<region>.kiro.dev`（控制面）。旧域名仍在代码里：`https://q.<region>.amazonaws.com`。

### 2.3 关键发现：官方留了端点覆盖开关（本方案的基础）

bundle 中原文（同一处连续代码）：

```js
function ajp(t,e,r){return r?e??[]:t?.globalValue??t?.defaultValue??[]}
function rfn(t){let e=Zpn.workspace.getConfiguration("codewhisperer.config");return ajp(e.inspect(t),e.get(t),Zpn.workspace.isTrusted)}
lxe=rfn("endpoints"),XGt=rfn("krsEndpoints"),ZGt=rfn("cpsEndpoints")
eHt=efn([...CGs,...lxe])
```

结论：

- **推理端点**读 `codewhisperer.config.krsEndpoints`（`XGt`），**控制面端点**读 `codewhisperer.config.cpsEndpoints`（`ZGt`），`codewhisperer.config.endpoints`（`lxe`）作为通用兜底。
- 这三个键**没有**声明在扩展 `package.json` 的 `contributes.configuration` 里（实测 0 处匹配），属于内部/未公开设置。但 VSCode 允许读取任意 settings.json 键，所以手写即可生效。
- `ajp` 的 `isTrusted` 分支意味着：工作区**未受信任**时只读 Global/默认值。**把值写在 User（Global）作用域可以绕开这个分支**，无论信任与否都生效。
- `efn` 有一个硬约束（原文）：`if(!e)throw new Error("Preset config array must include a us-east-1 entry for the default")`。**覆盖数组里必须包含一条 `us-east-1`，否则会抛异常。**
- 这些变量在**模块顶层求值**，改完设置必须**重载窗口**才生效。
- 值形如：`[{"region":"us-east-1","endpoint":"http://127.0.0.1:19800"}]`。
- **不要只写一条 `us-east-1`。** 参考实现 API2Kiro（`src/endpoints.ts`）的做法是把 Kiro 可能用的**全部 8 个 region 都映射到本机**（`us-east-1`、`eu-central-1`、`us-gov-east-1`、`us-gov-west-1`、`us-iso-east-1`、`us-isob-east-1`、`us-isof-south-1`、`us-isof-east-1`），这样无论 Kiro 选哪个 region 都会落到网关上，同时也自动满足 `efn` 的 us-east-1 硬约束。
- **必须写在 Global（User）作用域，并先清掉 workspace / workspaceFolder 级覆盖。** 因为 `ajp` 在窗口受信任时返回的是「生效值」（`e`），workspace 级的值优先级更高，会盖掉 Global。API2Kiro 的做法是先把 workspace / workspaceFolder 两级的该键删掉，再写 Global。

写入示例（`%APPDATA%\Kiro\User\settings.json`）：

```json
{
  "codewhisperer.config.endpoints":    [{"region":"us-east-1","endpoint":"http://127.0.0.1:19800"}],
  "codewhisperer.config.krsEndpoints": [{"region":"us-east-1","endpoint":"http://127.0.0.1:19800"}],
  "codewhisperer.config.cpsEndpoints": [{"region":"us-east-1","endpoint":"http://127.0.0.1:19801"}]
}
```

### 2.4 为什么不需要 MITM

- 在 bundle 里搜 `rejectUnauthorized` / `NODE_TLS_REJECT_UNAUTHORIZED` / `NODE_EXTRA_CA_CERTS` / `checkServerIdentity` / `pinnedCert` —— **没有** Kiro 自己的 TLS 校验或证书固定逻辑，走标准 HTTPS。
- 扩展宿主进程自己读代理环境变量（`https_proxy` / `http_proxy` / `all_proxy`），但主进程**没有**把设置里的 `http.proxy` 显式传进扩展宿主的 `env`；`--proxy-server` 只影响 Electron 渲染层。也就是说 MITM 这条路反而不如改设置可靠。
- 本机实测：`GET /ListAvailableModels`、`POST /generateAssistantResponse`、`application/vnd.amazon.eventstream` 均在 bundle 中确认存在。
- 本机日志实测推理端点：`%USERPROFILE%\.kiro\logs\20261014T064623546\kiro.log` 出现 `"endpoint":"https://runtime.us-east-1.kiro.dev"`。

### 2.5 已被验证的同类先例

以下项目都用「写 `codewhisperer.config.*Endpoints` 指向 127.0.0.1」这一招接管过 Kiro，说明这条路走得通：

- `TsinHzl/kiro2cc-proxy`（首创，翻译成 Anthropic Messages）
- `SunNorthGod/API2Kiro`（做成 VSCode 扩展，KRS 19800 + CPS 19801，激活时写设置、禁用时还原）
- `donyst/kiro-cli-pool-proxy`（Kiro CLI 侧的同类做法）
- `nopperabbo/kiroxy`、`hlmiao/kiro-ide-cli-proxy`
- `jwadow/kiro-gateway`（反向：把 Kiro API 暴露成 OpenAI/Anthropic 接口，含完整调用细节，是本方案协议部分的权威参考）

官方需求 issue：`kirodotdev/Kiro#9367`、`#8939`（BYOK）、`#10636`（自定义 LLM 代理 / base URL）。

---

## 3. 需要仿真的接口

### 3.1 必须实现

| 接口 | 说明 |
|---|---|
| `GET /ListAvailableModels`（控制面） | 必需，否则模型选择器为空。返回 `{models:[{modelId, modelName, description, modelProvider, tokenLimits:{maxInputTokens}, defaultModel:{modelId}}]}` |
| `POST /generateAssistantResponse`（推理，路径 A） | AWS JSON 1.0 请求体 + `application/vnd.amazon.eventstream` 二进制流式响应 |
| 或 `POST /v1/responses`（推理，路径 B） | JSON 请求体 + SSE 响应（OpenAI Responses 风格），当实验 `create_response_api` 生效时走这条 |

**两条推理路径都在代码里，必须都考虑**：`create_response_api` 实验默认值是 `"disabled"`（bundle 原文 `create_response_api:{default:"disabled"}`），但**本机日志显示该实验当前是开启的**（`Active experiments changed {"activeExperiments":"create_response_api=create_response"}`）。所以实现上应当以路径 A 为基线，路径 B 作为兼容分支。

请求头（路径 A，bundle 与 kiro-gateway 一致）：

```
Authorization: Bearer <Kiro 自己的 accessToken>
Content-Type: application/x-amz-json-1.0
x-amz-target: AmazonCodeWhispererStreamingService.GenerateAssistantResponse
x-amzn-kiro-agent-mode: vibe
x-amzn-kiro-profile-arn: <profileArn>
amz-sdk-invocation-id: <uuid>
User-Agent: ... KiroIDE-<version>-<fingerprint>
```

请求体（路径 A）核心结构：`{conversationState:{chatTriggerType, conversationId, currentMessage:{userInputMessage:{content, modelId, origin, userInputMessageContext:{tools, toolResults, editorState}}}, history:[...]}, profileArn}`。

路径 B 请求头（bundle 原文）：`Content-Type: application/json`、`Authorization: Bearer`、`x-amzn-kiro-origin`、`x-amzn-kiro-profile`；body 为 OpenAI Responses 格式（含 `model`、`reasoning:{type,effort,summary}`）。

### 3.2 event-stream 二进制帧格式

```
[totalLength u32][headersLength u32][preludeCRC u32][headers...][payload...][messageCRC u32]
header: [nameLen u8][name][valueType u8=7(string)][valueLen u16][value]
```

事件头：`:message-type: event`、`:event-type: <事件名>`、`:content-type: application/json`；异常帧用 `:message-type: exception` + `:exception-type`。

需要发出的事件类型：

| 事件 | 载荷 | 用途 |
|---|---|---|
| `assistantResponseEvent` | `{content, modelId}` | 正文流 |
| `reasoningContentEvent` | `{text, signature}` | 思考流 |
| `toolUseEvent` | `{toolUseId, name, input, stop}` | 工具调用（分片流式，末片 `stop:true`） |
| `messageMetadataEvent` | `{conversationId}` | 会话元数据 |
| `meteringEvent` / `metadataEvent` / `contextUsageEvent` | token 用量、`contextUsagePercentage` | 用量与上下文占用 |

### 3.3 建议实现（缺失会导致 UI 报错或功能不全）

`GetProfile` / `ListAvailableProfiles`（返回一个假 profile ARN 即可）、`GetUsageLimits`（返回 `{}`）、`GetUserPreference`。

### 3.4 可以省略，但有一个坑

`GET /featureConfiguration`、telemetry、cloud config（`app.kiro.dev`）可以返回空。

`InvokeMCP` / `InvokeMCPStream`（服务端 MCP 工具发现）可以不做任何真实工作，但**必须返回合法的 JSON-RPC 结果**（`initialize` → 协议版本与 capabilities、`tools/list` → `tools: []`、`prompts/list`、`resources/list` 等），并且 `id` 为 null 的通知请求要返回空 body。参考实现 API2Kiro 的注释明确记录了这一点：**直接返回 `{}` 会让 Kiro 的 RemoteToolsDiscovery 失败，agent 看起来「没有工具」**。Kiro 自己的文件/终端工具在客户端侧（ACP），不受影响，所以只要干净地告诉它「没有远程工具」，本地工具就正常可用。

注意这个请求会打到**运行时**端点（Kiro 把流式客户端的命令都发到 runtime），不是控制面端点。

**不需要实现设备码授权**：网关不负责登录，只复用用户已有的 Kiro 凭证（`~/.aws/sso/cache/kiro-auth-token.json`），或者干脆忽略请求里的 Authorization。

---

## 4. 在 cursor-byok 中的挂载点

### 4.1 可复用的抽象层

上游只有一个真正的 trait：

```rust
pub trait Provider: Send + Sync {
    fn stream(&self, invocation: ModelInvocation, cancellation: CancellationToken) -> ProviderStream;
}
```

`ProviderRouter` 已经把它收窄成「给一个 `ModelInvocation`，拿一条 `ModelEvent` 流」，并顺带提供超时、空闲看门狗、`llm_calls` 记账。**新模块只需要 `Arc<dyn Provider>` + `Store`。**

仓库有一条硬性架构约束（`cursor.md` 的「模块依赖」）：`run` / `provider` / `store` / `model` 禁止依赖 `cursor`。新模块应当只依赖这四者加 `network`，绝不反向依赖 `cursor`。

另外，`server/src/api/byok/` 已经有一套客户端无关的 OpenAI/Anthropic 兼容 API。但 **Kiro 说的是 CodeWhisperer / Responses 协议，用不上这个现成入口**，必须新写模块。

### 4.2 需要新增的文件

```
server/src/kiro/mod.rs          设置类型（KiroSettings / 端点 / 接管开关）+ 校验
server/src/kiro/wire.rs         event-stream 二进制帧编解码 + SSE 分支
server/src/kiro/request.rs      Kiro 请求 → ModelInvocation（含 history、tools、toolResults）
server/src/kiro/response.rs     ModelEvent → Kiro 事件帧（正文 / 思考 / 工具调用 / 用量）
server/src/kiro/gateway.rs      独立监听器（推理端口 + 控制面端口）
server/src/kiro/catalog.rs      /ListAvailableModels 响应，由模型库生成
server/src/kiro/host_settings.rs  写/还原 Kiro 的 settings.json（替代 Devin 的 host_patch）

server/src/control/kiro.rs      管理端点 handler

apps/desktop/src/features/kiro/KiroSettingsPage.tsx
apps/desktop/src/features/kiro/KiroSettingsPage.module.scss
```

### 4.3 需要修改的既有文件

| 文件 | 改动 |
|---|---|
| `server/src/lib.rs` | `pub mod kiro;` |
| `server/src/app.rs` | 构造 gateway；`merge` 状态路由；`serve_on` 里 `tokio::spawn(gateway.serve(...))` 并收尾 `abort()` |
| `server/src/control/mod.rs` | 注册 `/__byok-api__/api/kiro/...` 路由 |
| `server/src/control/service.rs` | 设置透传方法 |
| `server/src/store/settings.rs` | 新增 `kiro_router` 键 + get/set（复用 `service_settings` 表，**无需迁移**） |
| `server/src/network.rs` | 如需特殊出网客户端 |
| `apps/desktop/src/App.tsx` | 加 Route |
| `apps/desktop/src/shell/AppLayout.tsx` | 菜单项 + `keptAlivePages` |
| `apps/desktop/src/shared/api.ts` | 类型 + 方法 |
| `apps/desktop/src/shared/store/appStore.ts` | 状态字段 + refresh |
| `apps/desktop/src/shared/ui/icons.ts` | 图标 |
| `apps/desktop/src/i18n/locales/en-US.json`、`pt-BR.json` | 翻译词条 |
| `apps/desktop/src/demo/api.ts` | demo 模式 mock |

`apps/desktop/src-tauri/` **不需要改**（Devin 模块也没动它，管理接口全走 HTTP）。

### 4.4 可以直接借用 Devin 模块的设计

- `server/src/local_app/settings.rs` 已经实现了「原子写入客户端 settings.json + `settings_match()` 判断是否已接管」的完整模式，写 Kiro 设置时直接照搬这个形状，比 Devin 的补丁机制简单一个量级。
- `server/src/network.rs` 的 `devin_client()`（关闭自动解压以便原样转发）如果 Kiro 侧也需要原样转发，可参考。
- 设置类型 + 校验 + `#[serde(default)]` 的写法、控制面路由注册方式，都可以照抄 Devin。

---

## 5. 分阶段实施计划

**阶段 0：黑盒验证（先做，0.5–1 人天）**
用一个最小 stub（比如 30 行的 HTTP 服务）先把端点覆盖这条路跑通：写入三个设置 → 重载 Kiro 窗口 → 确认 Kiro 真的把请求打到 `127.0.0.1`。Kiro 内置了请求转储开关 `KIRO_DUMP_REQUESTS`（`KIRO_DUMP_REQUESTS_DIR`），**用它抓真实请求体，比猜字段可靠得多**。这一步同时确定当前走的是 `/v1/responses` 还是 `/generateAssistantResponse`。

**阶段 1：控制面（2–3 人天）**
`GET /ListAvailableModels` 由模型库生成响应；补齐 `GetProfile` / `GetUsageLimits` / `GetUserPreference`；`host_settings.rs` 实现设置写入与还原。

**阶段 2：推理主路径（3–5 人天）**
先实现 SSE 的 `/v1/responses`（结构更简单，且本机实验已开启），打通「Kiro 提问 → 你的模型 → 正文流回 Kiro」。

**阶段 3：兼容 event-stream 路径（3–4 人天）**
实现 `POST /generateAssistantResponse` 与二进制帧编解码，覆盖未开实验的 Kiro 版本，避免被上游实验开关翻车。

**阶段 4：能力对齐（3–5 人天）**
工具调用（`toolUseEvent` 分片语义）、思考流（`reasoningContentEvent`）、用量事件（`meteringEvent` / `contextUsageEvent`）。这一阶段最容易出细节问题，建议逐项对着抓包验证。

**阶段 5：界面（2–3 人天）**
Kiro 设置页（端点、端口、模型映射、接管开关）、接管状态显示、i18n 词条、demo mock。

**合计**：MVP（阶段 0–2 + 5）约 8–12 人天；完整约 15–20 人天。

---

## 6. 风险与未解问题

**风险**

1. **三个设置键是未公开的内部设置**，没有出现在扩展 `package.json` 里。Kiro 升级可能改名或移除，导致接管失效。需要做「Kiro 版本探测 + 设置项存在性校验」，失败时明确提示而不是静默不生效。
2. **`efn` 强制要求覆盖数组包含 `us-east-1`**，否则扩展直接抛异常。写设置时必须保证这一点。
3. **region 必须与账号 region 一致**（本机 `us-east-1`），不一致时 `ifn` 按 region 查找会落空。
4. **两条推理路径并存**，实验开关由服务端下发，可能随时切换。只实现一条会有被切换打挂的风险。
5. **明文 HTTP**：Kiro 在非 HTTPS 端点会打警告（`[create-response-raw] HTTP endpoint detected — bearer token will be sent over an unencrypted connection`），功能可用但要注意它会把 Kiro 自己的 token 发到本机。网关应当忽略或校验这个 token，不要把 Kiro 凭证落盘。
6. **协议细节只能靠抓包确认**：本次是从 bundle 静态分析 + 第三方项目交叉验证，没有真实抓包。

**未解问题（需要真实抓包）**

1. 本机实验已开启，实际走 `/v1/responses` 还是 `/generateAssistantResponse`——用 `KIRO_DUMP_REQUESTS` 确认。
2. `/v1/responses` 请求体的完整字段（目前只从代码确认了 `model` 和 `reasoning`）。
3. `GET /ListAvailableModels` 的查询参数细节与 `ListAvailableModelsRequest` 字段。
4. 工具调用在两条路径下的确切分片语义（`toolUseEvent` 的 `stop` 与 `input` 累积规则）。
5. Kiro 对 `assistantResponseEvent` 之外事件（如 `invalidStateEvent`、`binaryMetadataEvent`）的容错边界。

---

## 7. 参考证据索引

| 证据 | 位置 |
|---|---|
| 端点构造 | `extension.js` → `function eei(t,e){...https://${t}.${r}.kiro.dev...}` |
| 端点覆盖读取 | `extension.js` → `lxe=rfn("endpoints"),XGt=rfn("krsEndpoints"),ZGt=rfn("cpsEndpoints")` |
| us-east-1 硬约束 | `extension.js` → `function efn(t){...Preset config array must include a us-east-1 entry for the default...}` |
| 预设端点表 | `extension.js` → `ujp=[{region:"us-east-1",endpoint:"https://runtime.us-east-1.kiro.dev"},...]` |
| 模型列表 | `extension.js` → `["GET","/ListAvailableModels",200]` |
| 推理（旧路径） | `extension.js` → `["POST","/generateAssistantResponse",200]` |
| 推理（新路径） | `extension.js` → `l=new URL("/v1/responses",r)` |
| event-stream 判断 | `extension.js` → `u.includes("application/vnd.amazon.eventstream")` |
| 实验开关 | `extension.js` → `create_response_api:{default:"disabled"}` |
| 请求转储 | `extension.js` → `if(process.env.KIRO_DUMP_REQUESTS)` |
| 本机推理端点实测 | `%USERPROFILE%\.kiro\logs\20261014T064623546\kiro.log` |
| 本机实验状态实测 | `%USERPROFILE%\.kiro\logs\...\kiro.log` → `"activeExperiments":"create_response_api=create_response"` |
| 本机凭证 | `%USERPROFILE%\.aws\sso\cache\kiro-auth-token.json`（含 `refreshToken`、`profileArn`，敏感，勿外传） |
| 协议权威参考 | `jwadow/kiro-gateway`、`coderhisham/kiro-gateway-next`、`SunNorthGod/API2Kiro`、`TsinHzl/kiro2cc-proxy` |

---

## 8. 参考实现对照：SunNorthGod/API2Kiro

`SunNorthGod/API2Kiro` 是一个跑在 Kiro 内部的 VSCode 扩展，机制与本方案完全一致（写 `codewhisperer.config` 端点 + 本地起两个服务），已实际跑通。它是本方案「Kiro 侧」最直接的参考。但它有三处不能照抄。

### 8.1 它的结构（已读源码，Node/TS，约 2,900 行）

```
API2Kiro/
├── src/extension.ts          215  激活入口：起两个服务、写端点、状态栏
├── src/endpoints.ts          109  ★ 写 codewhisperer.config 三个键（8 个 region → 127.0.0.1，Global 作用域，先清 workspace 覆盖）
├── src/krsServer.ts          421  ★ 运行时 19800：收 generateAssistantResponse，转发上游，回 event-stream
├── src/cpsServer.ts          208  ★ 控制面 19801：模型列表、假 profile、GetUsageLimits
├── src/eventstream.ts         85  ★ AWS event-stream 二进制帧编码（含 CRC32 表）
├── src/cwTypes.ts            100  ★ CodeWhisperer 请求体与事件类型
├── src/cwEvents.ts            22  ★ 事件对象 → 二进制帧
├── src/translate.ts          376    CW 请求 → Anthropic 请求（模型映射、effort 档位）
├── src/anthropicStream.ts    240    Anthropic SSE → CW 事件（工具/思考/用量）
├── src/upstream.ts            94    上游 HTTP 发起
├── src/modelStore.ts         253    从上游 /v1/models 拉模型、按 effort 分组
├── src/effort.ts             162    思考档位映射
├── src/portBinder.ts         142    多窗口端口选举（主实例 / 接管）
├── src/config.ts             217    扩展配置
├── src/intentClassifier.ts    72    本地拦意图分类请求
├── src/usage.ts              184    用量 / 缓存命中率面板
├── src/sidebar.ts             ...    控制面板 UI
└── src/log.ts                 ...    日志
```

### 8.2 不能照抄的三处

**第一处：上游协议写死 Anthropic。** `krsServer.ts` 固定往 `<baseUrl>/messages` 发请求，带 `x-api-key` + `anthropic-version: 2023-06-01`，只认 Anthropic Messages 的 SSE。本项目的 `ProviderRouter` 已经支持 OpenAI Chat / Responses / Anthropic 三种协议并自带记账，所以这一整块（`translate.ts` + `anthropicStream.ts` + `upstream.ts` 约 710 行）应当被 `Arc<dyn Provider>` 取代——**照抄反而是倒退**。

**第二处：宿主形态不同。** API2Kiro 是跑在 Kiro 里的扩展：进程内起 HTTP 服务，用 `vscode.workspace.getConfiguration().update()` 写设置，还带一套多窗口端口选举（`portBinder.ts`，先启动的窗口当主实例、退出后其他窗口接管）。本项目是 Kiro 之外的独立进程，没有扩展宿主可用，必须**直接读写 `%APPDATA%\Kiro\User\settings.json`**。好消息是 `server/src/local_app/settings.rs` 已经实现了「原子写入 + `settings_match()` 判断是否已接管」这套模式，形状直接可复用。多窗口选举也不需要（应用本身是单例）。

**第三处：只覆盖了 `generateAssistantResponse`。** 它完全没有处理 `/v1/responses`。这是本方案最大的未知项，见 §6 风险 4。

### 8.3 它帮我们确认下来的事实（这些可以直接采信）

| 事实 | 出处 |
|---|---|
| 端点要写全部 8 个 region 到 127.0.0.1，写在 Global，并先清掉 workspace/workspaceFolder 覆盖 | `endpoints.ts` |
| event-stream 帧编码就是 §3.2 那个布局，CRC32 用标准多项式 `0xedb88320` | `eventstream.ts` |
| 控制面只需要三件事：`ListAvailableModels`、假 profile ARN（`arn:aws:codewhisperer:us-east-1:000000000000:profile/...`）、`GetUsageLimits` 返回 `{}`；其余一律回 `{}` | `cpsServer.ts` |
| `InvokeMCPCommand` 会打到**运行时**端点，必须回合法 JSON-RPC，回 `{}` 会导致 agent 认为没有工具 | `krsServer.ts` |
| 工具调用可以简化：不必分片，在 `content_block_stop` 一次性发出 `toolUseEvent` 并置 `stop: true` 即可（`cwEvents.ts` 里无条件补 `stop: true`） | `cwEvents.ts` + `anthropicStream.ts` |
| 思考流：`reasoningContentEvent` 先逐片发 `{text}`，结束时再发一帧 `{signature}` | `anthropicStream.ts` |
| 用量：`metadataEvent{type:"token_usage", inputTokens, outputTokens}`；上下文占用率走独立的 `contextUsageEvent{contextUsagePercentage}`，在流末尾 `flush()` 发出 | `anthropicStream.ts` |
| Kiro 有意图分类请求可以在本地直接拦掉，省一次上游调用（可选优化） | `intentClassifier.ts` |
| 模型选择器读的是 `ListAvailableModels` 的 `tokenLimits`、`additionalModelRequestFieldsSchema`（思考档位靠它）、`defaultModel` | `cpsServer.ts` |

### 8.4 本项目的对应结构（Rust，≈1,300 行，行数为估算目标，不含测试与空行）

```
server/src/kiro/
├── mod.rs             ≈150  设置类型 + 校验（对应 endpoints.ts 的类型部分）
├── wire.rs            ≈120  event-stream 帧编解码（对应 eventstream.ts）
├── request.rs         ≈250  Kiro 请求 → ModelInvocation（对应 cwTypes.ts + translate.ts 的请求侧）
├── response.rs        ≈300  ModelEvent → Kiro 事件（对应 cwEvents.ts + anthropicStream.ts）
├── gateway.rs         ≈250  运行时 + 控制面两个监听器（对应 krsServer.ts + cpsServer.ts）
├── catalog.rs         ≈150  /ListAvailableModels，由模型库生成（对应 cpsServer.ts 的模型分支 + modelStore.ts）
└── host_settings.rs   ≈200  直接写 %APPDATA%\Kiro\User\settings.json（对应 endpoints.ts）
```

**不需要移植的部分**（合计约 1,500 行 TS）：`translate.ts` / `anthropicStream.ts` / `upstream.ts` 由 `Provider` 层替代；`modelStore.ts` 由模型库替代；`portBinder.ts` / `config.ts` / `usage.ts` / `sidebar.ts` 由桌面端现有设置页与单例模型替代。

### 8.5 与 API2Kiro 共存（实测发现，必须处理）

本机 Kiro 里已经装了 **API2Kiro 1.7.7**（`~/.kiro/extensions/api2kiro.api2kiro-1.7.7`）。两者是同一个赛道、同一套机制：

| 维度 | API2Kiro | 本方案 |
|---|---|---|
| 接管方式 | 写 `codewhisperer.config.{endpoints,krsEndpoints,cpsEndpoints}` | 完全相同 |
| 默认运行时端口 | 19800 | **19700**（刻意避开） |
| 默认控制面端口 | 19801 | **19701**（刻意避开） |
| 身份探测 | `GET /__api2kiro_identity` 返回 `{proxy:"api2kiro",role:"krs"\|"cps"}` | 不实现，返回 404 |

**实测现象**：本方案的探针先占用了 19800/19801 之后，API2Kiro 的端口选举探针开始每 2.5 秒重试一次 `/__api2kiro_identity`，持续不停——它检测到端口被外来进程占用，但拿不到自己的身份标识，于是不断重试。两个产品同时启用会互相抢监听，谁都工作不正常。

由此得出三条设计要求：

1. **默认端口必须避开 19800/19801**，已改为 19700/19701（见 `KiroSettings::default`）。
2. **端口被占用时要明确报错**，不能静默抢监听。API2Kiro 靠身份探测做多窗口选举，本方案是单例应用，遇到占用应当直接把错误显示在设置页上，并提示「19800/19801 被 API2Kiro 占用」这类可操作的信息。
3. **不要冒充 API2Kiro 的身份标识**。返回 404 是正确的——API2Kiro 会据此判断端口被外来进程占用（它现在的行为就是这样）。冒充会让对方误以为端口归自己所有，掩盖冲突。

**未解**：本节记录的是「端口冲突」这一个现象。阶段 0 想回答的「Kiro 走哪条推理路径」**仍然没有答案**——那次探针运行期间没有收到任何聊天请求，只有 API2Kiro 的探测流量。需要换端口重做一次。
