import { useEffect, useRef, useState } from "react";
import { api, type Model } from "../../shared/api";
import { FormField } from "../../shared/ui/FormControls";
import { Modal } from "../../shared/ui/Modal";
import { Select } from "../../shared/ui/Select";
import { useMessage } from "../../shared/ui/message";
import controls from "../../shared/ui/Controls.module.scss";
import { CursorModelTestResult, type CursorModelTestState } from "./CursorModelTestResult";
import { readImageOptions, imageOptionsError, imageModelLabel, type ImageGenerationOptions } from "../../shared/utils/imageGeneration";
import { ImageGenerationOptionsFields } from "./ImageGenerationOptionsFields";
import styles from "./ImageModelTestDialog.module.scss";

export function ImageModelTestDialog({ models, initialModel, onClose }: {
  models: Model[];
  initialModel: string;
  onClose: () => void;
}) {
  const message = useMessage();
  const [modelHash, setModelHash] = useState(initialModel);
  const [prompt, setPrompt] = useState("");
  const [state, setState] = useState<CursorModelTestState>();
  const [testing, setTesting] = useState(false);
  const [options, setOptions] = useState<ImageGenerationOptions>({});
  const model = models.find((model) => model.model_hash === modelHash);
  let defaults: ImageGenerationOptions = {};
  let optionsError: string | null = null;
  try {
    if (model?.openai_extra_params_enabled) defaults = readImageOptions(model.openai_extra_params, model.openai_endpoint);
    optionsError = imageOptionsError({ ...defaults, ...options });
  } catch (cause) { optionsError = errorText(cause); }
  const active = useRef<{ hash: string; id: string; controller: AbortController } | null>(null);
  const cancel = () => {
    const test = active.current;
    if (!test || test.controller.signal.aborted) return;
    test.controller.abort();
    void api.cancelModelTest(test.hash, test.id).catch((cause) => {
      message(t("取消测试失败：{error}", { error: errorText(cause) }));
    });
  };
  useEffect(() => () => {
    const test = active.current;
    if (test && !test.controller.signal.aborted) {
      test.controller.abort();
      void api.cancelModelTest(test.hash, test.id).catch(() => {});
    }
  }, []);
  const run = async () => {
    if (active.current || !prompt.trim() || !model || optionsError) return;
    const test = { hash: modelHash, id: crypto.randomUUID(), controller: new AbortController() };
    active.current = test;
    setTesting(true);
    setState(undefined);
    try {
      const result = await api.testModel(test.hash, test.id, test.controller.signal, prompt.trim(), options);
      setState(test.controller.signal.aborted ? { status: "cancelled" } : { status: "success", result });
    } catch (cause) {
      setState(test.controller.signal.aborted ? { status: "cancelled" } : { status: "error", error: errorText(cause) });
    } finally {
      active.current = null;
      setTesting(false);
    }
  };
  return <Modal open title={t("生图测试")} busy={testing} onClose={onClose} onSubmit={() => void run()}
    submitLabel={t("开始生成")} submitDisabled={!prompt.trim() || !model || Boolean(optionsError)}
    secondaryAction={testing ? <button type="button" className={controls.secondary} onClick={cancel}>{t("取消测试")}</button> : undefined}>
    <div className={styles.form}>
      <FormField label={t("测试模型")} hint={t("仅用于本次测试，不改变默认生图模型。")}>
        <Select ariaLabel={t("测试模型")} value={modelHash} disabled={testing} options={models.map((model) => ({ value: model.model_hash, label: imageModelLabel(model) }))}
          onChange={(hash) => { setModelHash(hash); setOptions({}); setState(undefined); }} />
      </FormField>
      <ImageGenerationOptionsFields key={modelHash} value={options} defaults={defaults} disabled={testing}
        onChange={(patch) => { setOptions((current) => Object.fromEntries(Object.entries({ ...current, ...patch }).filter(([, value]) => value !== undefined))); setState(undefined); }} />
      {optionsError && <p role="alert">{optionsError}</p>}
      <FormField label={t("图片描述")}>
        <textarea className={styles.prompt} aria-label={t("图片描述")} rows={6} maxLength={16000} value={prompt} disabled={testing}
          placeholder={t("例如：一只坐在窗边的橘猫，水彩风格，柔和的午后阳光。")}
          onChange={(event) => setPrompt(event.target.value)} />
      </FormField>
      <small className={styles.hint}>{t("测试会产生服务用量。此处的描述仅用于测试，Cursor Agent 的生图描述仍由原生工具传入。")}</small>
      {(testing || state) && <CursorModelTestResult state={state} testing={testing} />}
    </div>
  </Modal>;
}

function errorText(cause: unknown) {
  return cause instanceof Error ? cause.message : String(cause);
}
