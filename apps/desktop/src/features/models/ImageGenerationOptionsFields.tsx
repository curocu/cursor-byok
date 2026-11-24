import { useState } from "react";
import type { ImageGenerationOptions } from "../../shared/utils/imageGeneration";
import { FormField, TextInput } from "../../shared/ui/FormControls";
import { Select } from "../../shared/ui/Select";
import styles from "./ImageGenerationOptionsFields.module.scss";

const sizes = ["auto", "1024x1024", "1536x1024", "1024x1536", "1536x864", "864x1536", "2048x2048", "2560x1440", "3840x2160"];

export function ImageGenerationOptionsFields({ value, defaults = {}, disabled, onChange }: {
  value: ImageGenerationOptions;
  defaults?: ImageGenerationOptions;
  disabled?: boolean;
  onChange: (patch: ImageGenerationOptions) => void;
}) {
  const [customSize, setCustomSize] = useState(false);
  const inherited = (key: keyof ImageGenerationOptions) => defaults[key] === undefined
    ? t("默认（不发送）") : t("继承模型配置：{value}", { value: defaults[key]! });
  const fields = [
    { key: "quality", label: t("画质"), values: ["auto", "low", "medium", "high", "xhigh", "max"] },
    { key: "output_format", label: t("图片格式"), values: ["png", "jpeg", "webp"] },
    { key: "background", label: t("图片背景"), values: ["auto", "opaque", "transparent"] },
    { key: "moderation", label: t("内容审核"), values: ["auto", "low"] },
    { key: "complexity", label: t("生成复杂度"), values: ["low", "medium", "high"] },
  ] as const;
  const labels: Record<string, string> = {
    auto: t("自动"), low: t("低"), medium: t("中"), high: t("高"), xhigh: t("超高（需上游支持）"), max: t("最高（需上游支持）"),
    opaque: t("不透明"), transparent: t("透明（需上游支持）"), png: "PNG", jpeg: "JPEG", webp: "WebP",
  };
  const size = value.size ?? "";
  const custom = customSize || Boolean(size && !sizes.includes(size));
  return <div className={styles.grid}>
    <FormField label={t("图片尺寸")} hint={t("预设不代表服务已支持。扩展尺寸、自定义分辨率和高档画质需上游支持；不按模型名称自动推断。") }>
      <Select ariaLabel={t("图片尺寸")} disabled={disabled} value={custom ? "custom" : size}
        options={[{ value: "", label: inherited("size") }, ...sizes.map((value) => ({ value, label: value === "auto" ? t("自动") : value.replace("x", " × ") })), { value: "custom", label: t("自定义尺寸") }]}
        onChange={(next) => { setCustomSize(next === "custom"); onChange({ size: next === "custom" ? (size && !sizes.includes(size) ? size : "") : next || undefined }); }} />
      {custom && <TextInput aria-label={t("自定义尺寸")} disabled={disabled} placeholder="1536x864" value={size} onChange={(event) => onChange({ size: event.target.value.trim() })} />}
    </FormField>
    {fields.map(({ key, label, values }) => {
      const current = value[key] ?? "";
      const choices: string[] = [...values];
      if (current && !choices.includes(current)) choices.push(current);
      return <FormField key={key} label={label}>
        <Select ariaLabel={label} disabled={disabled} value={current}
          options={[{ value: "", label: inherited(key) }, ...choices.map((value) => ({ value, label: key === "moderation" && value === "low" ? t("较宽松（low）") : labels[value] ?? value }))]}
          onChange={(value) => onChange({ [key]: value || undefined })} />
      </FormField>;
    })}
    <FormField label={t("压缩率")} hint={t("仅 JPEG / WebP，0–100；保留空值继承模型配置或上游默认。") }>
      <TextInput aria-label={t("压缩率")} type="number" min={0} max={100} step={1} disabled={disabled}
        placeholder={inherited("output_compression")} value={value.output_compression ?? ""}
        onChange={(event) => onChange({ output_compression: event.target.value === "" ? undefined : Number(event.target.value) })} />
    </FormField>
  </div>;
}
