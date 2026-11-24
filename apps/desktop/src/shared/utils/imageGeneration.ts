import type { ModelInput } from "../api";

export function isImageModel(model: Pick<ModelInput, "type" | "openai_endpoint">) {
  return model.type === "openai" && ["/v1/images/generations", "responses-image-generation"].includes(model.openai_endpoint);
}

/** Image model ids are not unique across protocols; include the id and protocol in pickers. */
export function imageModelLabel(model: Pick<ModelInput, "display_name" | "model_id" | "openai_endpoint">) {
  const name = model.display_name || model.model_id;
  const id = model.model_id && model.model_id !== name ? `（${model.model_id}）` : "";
  const protocol = model.openai_endpoint === "responses-image-generation" ? " · Responses" : " · Images";
  return `${name}${id}${protocol}`;
}

export interface ImageGenerationOptions {
  size?: string;
  quality?: string;
  output_format?: string;
  background?: string;
  output_compression?: number;
  moderation?: string;
  complexity?: string;
}

const optionKeys = ["size", "quality", "output_format", "background", "output_compression", "moderation", "complexity"] as const;

function imageOptionsTarget(extra: Record<string, unknown>, endpoint: string): Record<string, unknown> {
  if (endpoint === "/v1/images/generations") return extra;
  if (extra.tools === undefined) return {};
  const tools = extra.tools;
  if (!Array.isArray(tools) || tools.length !== 1 || !tools[0] || typeof tools[0] !== "object" || tools[0].type !== "image_generation") {
    throw new Error(t("请先修正高级参数：Responses 需要且只能包含一个 image_generation 工具。"));
  }
  return tools[0];
}

export function readImageOptions(extra: Record<string, unknown>, endpoint: string): ImageGenerationOptions {
  const target = imageOptionsTarget(extra, endpoint);
  return Object.fromEntries(optionKeys.filter((key) => target[key] !== undefined && target[key] !== null).map((key) => [key, target[key]]));
}

export function writeImageOptions(extra: Record<string, unknown>, endpoint: string, patch: ImageGenerationOptions): Record<string, unknown> {
  const next = structuredClone(extra);
  if (endpoint !== "/v1/images/generations" && next.tools === undefined) next.tools = [{ type: "image_generation" }];
  const target = imageOptionsTarget(next, endpoint);
  for (const key of optionKeys) {
    if (!(key in patch)) continue;
    if (patch[key] === undefined) delete target[key];
    else target[key] = patch[key];
  }
  return next;
}

export function imageOptionsError(options: ImageGenerationOptions): string | null {
  if (options.size !== undefined && (typeof options.size !== "string" || (options.size !== "auto" && (!/^\d+x\d+$/.test(options.size)
    || options.size.split("x").some((value) => Number(value) <= 0 || Number(value) > 8192))))) {
    return t("尺寸请输入正整数宽x高，例如 1536x864；单边不超过当前解码上限 8192。支持范围由上游决定。");
  }
  if (options.output_compression !== undefined && (!Number.isInteger(options.output_compression) || options.output_compression < 0 || options.output_compression > 100)) return t("压缩率必须是 0–100 的整数。");
  const format = options.output_format ?? "png";
  if (format === "jpeg" && options.background === "transparent") return t("透明背景需要 PNG 或 WebP 格式。");
  if (format === "png" && options.output_compression !== undefined) return t("压缩率仅适用于 JPEG 或 WebP，请清空压缩率或更换格式。");
  return null;
}

/**
 * Moves image options between the Images API root and the Responses image tool
 * when the user changes protocol in the editor. Unknown fields stay untouched.
 */
export function migrateImageOptions(extra: Record<string, unknown>, from: string, to: string): Record<string, unknown> {
  const next = structuredClone(extra);
  if (from === to) return next;
  if (from === "/v1/images/generations" && to === "responses-image-generation") {
    const moved: Array<[keyof ImageGenerationOptions, unknown]> = [];
    for (const key of optionKeys) {
      if (next[key] !== undefined) {
        moved.push([key, next[key]]);
        delete next[key];
      }
    }
    if (next.tools === undefined) next.tools = [{ type: "image_generation" }];
    const target = imageOptionsTarget(next, to);
    for (const [key, value] of moved) {
      if (target[key] === undefined) target[key] = value;
    }
    return next;
  }
  if (from === "responses-image-generation" && to === "/v1/images/generations") {
    if (next.tools === undefined) return next;
    const tools = next.tools;
    if (!Array.isArray(tools) || tools.length !== 1 || !tools[0] || typeof tools[0] !== "object" || tools[0].type !== "image_generation") {
      throw new Error(t("请先修正高级参数：Responses 需要且只能包含一个 image_generation 工具。"));
    }
    const tool = tools[0] as Record<string, unknown>;
    for (const key of optionKeys) {
      if (tool[key] !== undefined) next[key] = tool[key];
    }
    delete next.tools;
    return next;
  }
  return next;
}
