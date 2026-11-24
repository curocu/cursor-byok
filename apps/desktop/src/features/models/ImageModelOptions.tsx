import { readImageOptions, writeImageOptions, imageOptionsError, type ImageGenerationOptions } from "../../shared/utils/imageGeneration";
import { JsonEditor } from "../../shared/ui/JsonEditor";
import { Switch } from "../../shared/ui/Switch";
import type { CursorModelDraft } from "./CursorModelEditor";
import { ImageGenerationOptionsFields } from "./ImageGenerationOptionsFields";
import styles from "./CursorSettings.module.scss";

export function ImageModelOptions({ draft, onChange }: { draft: CursorModelDraft; onChange: (draft: CursorModelDraft) => void }) {
  let extra: Record<string, unknown> = {};
  let options: ImageGenerationOptions = {};
  let parseError: string | null = null;
  try {
    const parsed: unknown = JSON.parse(draft.openAIExtraParamsText || "{}");
    if (!parsed || typeof parsed !== "object" || Array.isArray(parsed)) throw new Error(t("OpenAI 额外参数必须是 JSON 对象。"));
    extra = parsed as Record<string, unknown>;
    options = readImageOptions(extra, draft.model.openai_endpoint);
  } catch (cause) { parseError = cause instanceof Error ? cause.message : String(cause); }
  const update = (patch: ImageGenerationOptions) => onChange({ ...draft, openAIExtraParamsText: JSON.stringify(writeImageOptions(extra, draft.model.openai_endpoint, patch), null, 2) });
  return <div className={`${styles.fullWidth} ${styles.jsonOption}`}>
    <label><span>{t("生图参数")}</span><Switch label={t("生图参数")} checked={draft.model.openai_extra_params_enabled}
      onChange={(openai_extra_params_enabled) => onChange({ ...draft, model: { ...draft.model, openai_extra_params_enabled } })} /></label>
    {draft.model.openai_extra_params_enabled && <>
      {parseError ? <p role="alert">{parseError}</p> : <>
        <ImageGenerationOptionsFields value={options} onChange={update} />
        {imageOptionsError(options) && <p role="alert">{imageOptionsError(options)}</p>}
      </>}
      <p>{t("模型默认参数；Cursor 明确指定宽高比时优先沿用原生请求。未选择的参数不强制发送。")}</p>
      <details><summary>{t("高级参数 JSON")}</summary>
        <JsonEditor ariaLabel={t("OpenAI 额外参数")} value={draft.openAIExtraParamsText} onChange={(openAIExtraParamsText) => onChange({ ...draft, openAIExtraParamsText })} />
      </details>
    </>}
  </div>;
}
