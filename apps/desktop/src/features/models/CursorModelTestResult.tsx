import type { ModelConnectivityResult } from "../../shared/api";
import { useState } from "react";
import { Icon } from "../../shared/ui/Icon";
import { Modal } from "../../shared/ui/Modal";
import { TooltipTrigger } from "../../shared/ui/TooltipTrigger";
import { informationOutlineIcon } from "../../shared/ui/icons";
import styles from "./CursorModelTestResult.module.scss";

export type CursorModelTestState =
  | { status: "success"; result: ModelConnectivityResult }
  | { status: "error"; error: string }
  | { status: "cancelled" };

export function CursorModelTestResult({ state, testing = false, compact = false }: {
  state?: CursorModelTestState;
  testing?: boolean;
  /** 列表行内的紧凑徽标形态:未测试时不渲染,详情放入悬浮提示。 */
  compact?: boolean;
}) {
  if (testing) {
    return compact
      ? <span className={`${styles.compact} ${styles.testing}`}>{t("测试中…")}</span>
      : <div className={`${styles.root} ${styles.testing}`}><span className={styles.summary}>{t("测试中…")}</span></div>;
  }
  if (!state) {
    return compact ? null : <div className={`${styles.root} ${styles.idle}`}><span className={styles.summary}>{t("未测试")}</span></div>;
  }
  if (state.status === "cancelled") {
    return compact
      ? <span className={`${styles.compact} ${styles.idle}`}>{t("测试已取消")}</span>
      : <div className={`${styles.root} ${styles.idle}`}><span className={styles.summary}>{t("测试已取消")}</span></div>;
  }

  if (state.status === "success" && state.result.image) {
    return <ImageTestResult image={state.result.image} duration={state.result.duration_ms} compact={compact} />;
  }

  const success = state.status === "success";
  const summary = success
    ? t("速度：{speed} tokens/s", { speed: formatSpeed(state.result.tokens_per_second) })
    : t("错误：{error}", { error: state.error });
  const detail = success
    ? t("速度 {speed} tokens/s · 首字 {firstText} ms · 总耗时 {duration} ms · 输出 {tokens} tokens{estimated} · 返回：{output}", {
      speed: formatSpeed(state.result.tokens_per_second),
      firstText: state.result.first_valid_response_ms ?? "--",
      duration: state.result.duration_ms,
      tokens: state.result.output_tokens,
      estimated: state.result.tokens_estimated ? t("（估算）") : "",
      output: state.result.output || "--",
    })
    : t("测试失败：{error}", { error: state.error });

  if (compact) {
    return <TooltipTrigger label={detail}>
      <span className={`${styles.compact} ${success ? styles.success : styles.error}`}>
        {success ? `${formatSpeed(state.result.tokens_per_second)} tokens/s` : t("测试失败")}
        <Icon icon={informationOutlineIcon} size="1em" />
      </span>
    </TooltipTrigger>;
  }

  return <div className={`${styles.root} ${success ? styles.success : styles.error}`}>
    <span className={styles.summary}>{summary}</span>
    <TooltipTrigger label={detail}><button type="button" className={styles.details}>{t("查看详情")}<Icon icon={informationOutlineIcon} size="1.1em" /></button></TooltipTrigger>
  </div>;
}

function ImageTestResult({ image, duration, compact }: {
  image: NonNullable<ModelConnectivityResult["image"]>;
  duration: number;
  compact: boolean;
}) {
  const [open, setOpen] = useState(false);
  return <>
    <div className={`${compact ? styles.compact : styles.root} ${styles.success}`}>
      <span className={styles.summary}>{t("生图成功")}</span>
      <button type="button" className={`${styles.details} ${styles.previewButton}`} onClick={() => setOpen(true)}>{t("查看图片")}</button>
    </div>
    <Modal wide open={open} title={t("生图测试预览")} closeLabel={t("关闭")} onClose={() => setOpen(false)}>
      <div className={styles.preview}>
        <img className={styles.previewImage} src={`data:${image.mime_type};base64,${image.image_data}`} alt={t("生图测试预览")} />
        <span>{image.width} × {image.height} · {image.mime_type} · {duration} ms</span>
      </div>
    </Modal>
  </>;
}

function formatSpeed(value: number) {
  return Number.isFinite(value) ? value.toFixed(1) : "0.0";
}
