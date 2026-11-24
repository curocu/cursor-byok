import { useEffect, useState } from "react";
import { api, type Model } from "../../shared/api";
import { useAppStore } from "../../shared/store/appStore";
import { imageModelLabel, isImageModel } from "../../shared/utils/imageGeneration";
import { Button } from "../../shared/ui/Button";
import { Modal } from "../../shared/ui/Modal";
import { Select } from "../../shared/ui/Select";
import { TitledCard } from "../../shared/ui/TitledCard";
import { useMessage } from "../../shared/ui/message";
import { CursorSettingsPage } from "../models/CursorSettingsPage";
import { ImageModelTestDialog } from "../models/ImageModelTestDialog";
import styles from "./ImageGenerationSettingsCard.module.scss";

export function ImageGenerationSettingsCard() {
  const { models: allModels } = useAppStore();
  const models = allModels.filter(isImageModel);
  const message = useMessage();
  const [settings, setSettings] = useState<{ model_hash: string } | null>(null);
  const [saving, setSaving] = useState(false);
  const [managing, setManaging] = useState(false);
  const [testingModel, setTestingModel] = useState<string | null>(null);
  useEffect(() => {
    let active = true;
    void api.imageGenerationSettings().then((value) => { if (active) setSettings(value); })
      .catch((cause) => { if (active) message(errorText(cause)); });
    return () => { active = false; };
  }, [allModels, message]);
  const saveDefault = async (hash: string) => {
    setSaving(true);
    try { setSettings(await api.setImageGenerationSettings(hash)); }
    catch (cause) { message(errorText(cause)); }
    finally { setSaving(false); }
  };
  const openTest = (model?: Model) => setTestingModel(model?.model_hash || settings?.model_hash || models[0]?.model_hash || "");
  return <>
    <TitledCard title={t("图片生成")} action={<div className={styles.actions}>
      <Button size="small" onClick={() => setManaging(true)}>{t("管理模型")}</Button>
      <Button size="small" disabled={!models.length} onClick={() => openTest()}>{t("生图测试")}</Button>
    </div>}>
      <div className={styles.content}>
        <div className={styles.row}>
          <strong>{t("默认生图模型")}</strong>
          <div className={styles.select}><Select ariaLabel={t("默认生图模型")} value={settings?.model_hash ?? ""} disabled={!settings || saving}
            options={[{ value: "", label: t("未启用 BYOK 生图（沿用官方转发）") }, ...models.map((model) => ({ value: model.model_hash, label: imageModelLabel(model) }))]}
            onChange={(hash) => void saveDefault(hash)} /></div>
        </div>
        <small>{t("用于 Cursor 原生 GenerateImage；生图模型不出现在聊天模型菜单。")}</small>
      </div>
    </TitledCard>
    <Modal wide fullHeight open={managing} title={t("管理生图模型")} closeLabel={t("关闭")} onClose={() => setManaging(false)}>
      <CursorSettingsPage imageGeneration onImageTest={openTest} />
    </Modal>
    {testingModel !== null && <ImageModelTestDialog models={models} initialModel={testingModel} onClose={() => setTestingModel(null)} />}
  </>;
}

function errorText(cause: unknown) {
  return cause instanceof Error ? cause.message : String(cause);
}
