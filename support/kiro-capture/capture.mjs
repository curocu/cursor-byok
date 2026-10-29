/**
 * Kiro 接入探针（阶段 0 验证用，一次性工具）
 *
 * 目的：在写真正的网关之前，用最小的代码回答三个问题
 *   1. codewhisperer.config 端点覆盖是否真的生效（Kiro 是否把请求打到本机）
 *   2. Kiro 走的是 /generateAssistantResponse（AWS event-stream）还是 /v1/responses（SSE）
 *   3. 请求体的真实字段是什么（尤其是 history / tools / toolResults）
 *
 * 做法：备份并改写 %APPDATA%\Kiro\User\settings.json 的三个端点键，在本机起两个监听，
 * 把收到的每一个请求原样落盘，然后回一个可识别的假响应。
 *
 * 用法：
 *   node support/kiro-capture/capture.mjs            写设置 + 起监听（Ctrl+C 退出）
 *   node support/kiro-capture/capture.mjs --restore  还原 settings.json
 *
 * 注意：落盘的请求体包含 Kiro 发来的编辑器上下文，可能含你的源码。目录可用
 * KIRO_CAPTURE_DIR 覆盖，默认在 support/kiro-capture/dumps/。
 */

import http from "node:http";
import fs from "node:fs";
import path from "node:path";
import os from "node:os";
import { fileURLToPath } from "node:url";

const HERE = path.dirname(fileURLToPath(import.meta.url));
const SETTINGS_PATH = path.join(
  process.env.APPDATA || path.join(os.homedir(), "AppData", "Roaming"),
  "Kiro",
  "User",
  "settings.json"
);
const BACKUP_PATH = `${SETTINGS_PATH}.kiro-capture.bak`;
const DUMP_DIR = process.env.KIRO_CAPTURE_DIR || path.join(HERE, "dumps");

// 刻意避开 API2Kiro 的默认端口 19800/19801：两者接管同一套设置键，共用端口会
// 变成互相抢监听（实测 API2Kiro 的端口选举探针会每 2.5 秒重试不停）。
const RUNTIME_PORT = 19700;
const CPS_PORT = 19701;

/** Kiro 可能选用的全部 region，全部指向本机（同时满足扩展对 us-east-1 的硬要求）。 */
const ALL_REGIONS = [
  "us-east-1",
  "eu-central-1",
  "us-gov-east-1",
  "us-gov-west-1",
  "us-iso-east-1",
  "us-isob-east-1",
  "us-isof-south-1",
  "us-isof-east-1",
];

const CW_CONFIG = "codewhisperer.config";
const EVENT_STREAM_CONTENT_TYPE = "application/vnd.amazon.eventstream";

// ---------------------------------------------------------------- event-stream

const CRC32_TABLE = (() => {
  const table = new Uint32Array(256);
  for (let i = 0; i < 256; i++) {
    let c = i;
    for (let k = 0; k < 8; k++) c = c & 1 ? (c >>> 1) ^ 0xedb88320 : c >>> 1;
    table[i] = c >>> 0;
  }
  return table;
})();

function crc32(buf, start, end) {
  let crc = 0xffffffff;
  for (let i = start; i < end; i++) crc = CRC32_TABLE[(crc ^ buf[i]) & 0xff] ^ (crc >>> 8);
  return (crc ^ -1) >>> 0;
}

function encodeHeader(name, value) {
  const n = Buffer.from(name, "utf8");
  const v = Buffer.from(value, "utf8");
  const buf = Buffer.alloc(1 + n.length + 1 + 2 + v.length);
  let off = 0;
  buf.writeUInt8(n.length, off);
  off += 1;
  n.copy(buf, off);
  off += n.length;
  buf.writeUInt8(7, off); // value type 7 = string
  off += 1;
  buf.writeUInt16BE(v.length, off);
  off += 2;
  v.copy(buf, off);
  return buf;
}

function encodeFrame(headers, payload) {
  const total = 12 + headers.length + payload.length + 4;
  const buf = Buffer.alloc(total);
  let off = 0;
  buf.writeUInt32BE(total, off);
  off += 4;
  buf.writeUInt32BE(headers.length, off);
  off += 4;
  buf.writeUInt32BE(crc32(buf, 0, 8), off);
  off += 4;
  headers.copy(buf, off);
  off += headers.length;
  payload.copy(buf, off);
  off += payload.length;
  buf.writeUInt32BE(crc32(buf, 0, off), off);
  return buf;
}

function encodeEvent(eventType, payload) {
  const headers = Buffer.concat([
    encodeHeader(":message-type", "event"),
    encodeHeader(":event-type", eventType),
    encodeHeader(":content-type", "application/json"),
  ]);
  return encodeFrame(headers, Buffer.from(JSON.stringify(payload), "utf8"));
}

// ------------------------------------------------------------------- settings

function readSettings() {
  try {
    return JSON.parse(fs.readFileSync(SETTINGS_PATH, "utf8"));
  } catch (e) {
    if (e.code === "ENOENT") return {};
    throw new Error(`无法解析 ${SETTINGS_PATH}：${e.message}`);
  }
}

function writeSettings(settings) {
  const tmp = `${SETTINGS_PATH}.kiro-capture.tmp`;
  fs.mkdirSync(path.dirname(SETTINGS_PATH), { recursive: true });
  fs.writeFileSync(tmp, `${JSON.stringify(settings, null, 2)}\n`, "utf8");
  fs.renameSync(tmp, SETTINGS_PATH);
}

function endpointList(port) {
  return ALL_REGIONS.map((region) => ({ region, endpoint: `http://127.0.0.1:${port}` }));
}

function applyOverrides() {
  if (!fs.existsSync(BACKUP_PATH)) {
    if (fs.existsSync(SETTINGS_PATH)) {
      fs.copyFileSync(SETTINGS_PATH, BACKUP_PATH);
      console.log(`已备份原设置 → ${BACKUP_PATH}`);
    } else {
      fs.writeFileSync(BACKUP_PATH, "{}\n", "utf8");
      console.log(`原设置不存在，已建立空备份 → ${BACKUP_PATH}`);
    }
  }
  const settings = readSettings();
  settings[`${CW_CONFIG}.endpoints`] = endpointList(RUNTIME_PORT);
  settings[`${CW_CONFIG}.krsEndpoints`] = endpointList(RUNTIME_PORT);
  settings[`${CW_CONFIG}.cpsEndpoints`] = endpointList(CPS_PORT);
  writeSettings(settings);
  console.log(`已写入 ${CW_CONFIG}.endpoints / krsEndpoints / cpsEndpoints → 127.0.0.1`);
}

function restore() {
  if (!fs.existsSync(BACKUP_PATH)) {
    console.log(`没有找到备份 ${BACKUP_PATH}，无需还原。`);
    return;
  }
  const settings = readSettings();
  for (const key of ["endpoints", "krsEndpoints", "cpsEndpoints"]) {
    delete settings[`${CW_CONFIG}.${key}`];
  }
  const original = JSON.parse(fs.readFileSync(BACKUP_PATH, "utf8"));
  writeSettings({ ...settings, ...original });
  fs.unlinkSync(BACKUP_PATH);
  console.log(`已还原 ${SETTINGS_PATH}，并删除备份。`);
}

// ---------------------------------------------------------------------- dumps

let dumpSeq = 0;

function dump(role, req, body) {
  fs.mkdirSync(DUMP_DIR, { recursive: true });
  const seq = String(++dumpSeq).padStart(3, "0");
  const stamp = new Date().toISOString().replace(/[:.]/g, "-");
  const base = path.join(DUMP_DIR, `${stamp}-${seq}-${role}`);

  let parsed = null;
  try {
    parsed = JSON.parse(body);
  } catch {
    /* 非 JSON，保留原文 */
  }

  fs.writeFileSync(
    `${base}.json`,
    `${JSON.stringify(
      {
        role,
        method: req.method,
        url: req.url,
        headers: req.headers,
        bodyBytes: Buffer.byteLength(body),
        body: parsed ?? body,
      },
      null,
      2
    )}\n`,
    "utf8"
  );
  if (parsed !== null) fs.writeFileSync(`${base}.body`, body, "utf8");

  const op = req.headers["x-amz-target"] || "";
  console.log(
    `[${role}] ${req.method} ${req.url}${op ? `  x-amz-target=${op}` : ""}  ${Buffer.byteLength(body)}B  → ${path.basename(base)}.json`
  );
  return base;
}

// -------------------------------------------------------------------- servers

function readBody(req) {
  return new Promise((resolve) => {
    const chunks = [];
    req.on("data", (c) => chunks.push(c));
    req.on("end", () => resolve(Buffer.concat(chunks).toString("utf8")));
  });
}

function jsonRpcReply(body) {
  let id = null;
  let method = "";
  try {
    const parsed = JSON.parse(body);
    id = parsed?.id ?? null;
    method = typeof parsed?.method === "string" ? parsed.method : "";
  } catch {
    /* 无法解析时按通用结果回复 */
  }
  if (id === null || id === undefined) return null; // 通知，无需响应体
  const result =
    method === "initialize"
      ? { protocolVersion: "2024-11-05", capabilities: { tools: {} }, serverInfo: { name: "kiro-capture", version: "0.0.0" } }
      : method === "tools/list"
        ? { tools: [] }
        : method === "prompts/list"
          ? { prompts: [] }
          : method === "resources/list"
            ? { resources: [] }
            : method === "resources/templates/list"
              ? { resourceTemplates: [] }
              : {};
  return JSON.stringify({ jsonrpc: "2.0", id, result });
}

function startRuntimeServer() {
  const server = http.createServer(async (req, res) => {
    const body = await readBody(req);
    const base = dump("runtime", req, body);
    const p = (req.url || "/").split("?")[0];

    // 服务端 MCP 工具发现：必须回合法 JSON-RPC，否则 Kiro 认为 agent 没有工具。
    if (body.includes('"jsonrpc"') && body.includes('"method"')) {
      const reply = jsonRpcReply(body);
      res.writeHead(200, { "Content-Type": "application/json" });
      res.end(reply ?? "");
      return;
    }

    // 新路径：OpenAI Responses 风格 SSE。
    if (p === "/v1/responses") {
      res.writeHead(200, {
        "Content-Type": "text/event-stream",
        "Cache-Control": "no-cache",
        Connection: "keep-alive",
      });
      const send = (type, data) => res.write(`event: ${type}\ndata: ${JSON.stringify(data)}\n\n`);
      send("response.created", { type: "response.created", response: { id: "capture-probe", model: "capture-probe" } });
      send("response.output_item.added", { type: "response.output_item.added", output_index: 0, item: { id: "msg_0", type: "message", role: "assistant" } });
      for (const chunk of ["✅ 探针已收到 ", "/v1/responses ", "请求。"]) {
        send("response.output_text.delta", { type: "response.output_text.delta", output_index: 0, delta: chunk });
      }
      send("response.output_item.done", { type: "response.output_item.done", output_index: 0, item: { id: "msg_0", type: "message", role: "assistant" } });
      send("response.completed", { type: "response.completed", response: { id: "capture-probe", status: "completed" } });
      res.end();
      return;
    }

    // 旧路径：AWS event-stream 二进制帧。
    res.writeHead(200, { "Content-Type": EVENT_STREAM_CONTENT_TYPE, "Transfer-Encoding": "chunked" });
    res.socket?.setNoDelay(true);
    res.write(encodeEvent("messageMetadataEvent", { conversationId: "capture-probe" }));
    res.write(
      encodeEvent("assistantResponseEvent", {
        content: "✅ 探针已收到 /generateAssistantResponse 请求，说明端点覆盖生效。",
        modelId: "capture-probe",
      })
    );
    res.end();
    void base;
  });
  return server;
}

function startCpsServer() {
  const model = {
    modelId: "capture-probe",
    modelName: "Capture Probe",
    description: "阶段 0 抓包探针",
    promptCaching: {
      maximumCacheCheckpointsPerRequest: 4,
      minimumTokensPerCacheCheckpoint: 1024,
      supportsPromptCaching: true,
    },
    rateUnit: "Credit",
    supportedInputTypes: ["TEXT", "IMAGE"],
    tokenLimits: { maxInputTokens: 200000, maxOutputTokens: 64000 },
  };

  const server = http.createServer(async (req, res) => {
    const body = await readBody(req);
    dump("cps", req, body);

    const p = (req.url || "/").split("?")[0];
    const target = String(req.headers["x-amz-target"] || req.headers["x-amzn-target"] || "");
    const op = target.split(".").pop()?.replace(/[^a-z0-9]/gi, "").toLowerCase() || "";
    const key = p.replace(/[^a-z0-9]/gi, "").toLowerCase();
    const contentType = target ? "application/x-amz-json-1.0" : "application/json";

    const reply = (payload) => {
      res.writeHead(200, { "Content-Type": contentType });
      res.end(typeof payload === "string" ? payload : JSON.stringify(payload));
    };

    if (op === "listavailablemodels" || key.includes("availablemodels")) {
      reply({ models: [model], defaultModel: { modelId: model.modelId } });
      return;
    }
    if (op.includes("profile") || key.includes("profile")) {
      const profile = {
        arn: "arn:aws:codewhisperer:us-east-1:000000000000:profile/KIROCAPTURE",
        profileName: "Kiro Capture",
        identityDetails: { region: "us-east-1" },
      };
      reply(key.includes("list") ? { profiles: [profile] } : { profile });
      return;
    }
    if (op === "getusagelimits" || key.includes("usagelimit")) {
      reply({});
      return;
    }
    reply({});
  });
  return server;
}

// ----------------------------------------------------------------------- main

if (process.argv.includes("--restore")) {
  restore();
  process.exit(0);
}

console.log(`设置文件：${SETTINGS_PATH}`);
console.log(`转储目录：${DUMP_DIR}`);
applyOverrides();

const runtime = startRuntimeServer();
const cps = startCpsServer();

runtime.listen(RUNTIME_PORT, "127.0.0.1", () =>
  console.log(`运行时监听     http://127.0.0.1:${RUNTIME_PORT}`)
);
cps.listen(CPS_PORT, "127.0.0.1", () => console.log(`控制面监听     http://127.0.0.1:${CPS_PORT}`));

console.log(
  "\n下一步：完全退出 Kiro 再重新打开（端点在扩展加载时求值），新建对话、选择 “Capture Probe” 模型并发送任意消息。"
);
console.log("结束后用 Ctrl+C 退出，再执行：node support/kiro-capture/capture.mjs --restore\n");
