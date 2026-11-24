//! Persists the explicit default used by Cursor's native image tool.
use serde::{Deserialize, Serialize};

use super::{now_ms, Store};
use crate::{Error, Result};

pub(super) const IMAGE_SETTINGS_KEY: &str = "image_generation";

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct ImageGenerationSettings {
    /// Empty preserves the original official forwarding path.
    pub model_hash: String,
}

impl Store {
    pub async fn image_generation_settings(&self) -> Result<ImageGenerationSettings> {
        let value = sqlx::query_scalar::<_, String>(
            "SELECT value_json FROM service_settings WHERE setting_key = ?",
        )
        .bind(IMAGE_SETTINGS_KEY)
        .fetch_optional(&self.pool)
        .await?;
        value
            .map(|value| serde_json::from_str(&value).map_err(Into::into))
            .unwrap_or_else(|| Ok(ImageGenerationSettings::default()))
    }

    pub async fn set_image_generation_settings(
        &self,
        settings: ImageGenerationSettings,
    ) -> Result<ImageGenerationSettings> {
        let _write = self.writes.lock().await;
        let settings = ImageGenerationSettings {
            model_hash: settings.model_hash.trim().into(),
        };
        if !settings.model_hash.is_empty() {
            let model = self
                .model(&settings.model_hash)
                .await?
                .ok_or_else(|| Error::Config("image generation model is not configured".into()))?;
            if !model.is_image_generation() {
                return Err(Error::Config(
                    "select an image generation protocol model".into(),
                ));
            }
        }
        sqlx::query("INSERT INTO service_settings(setting_key, value_json, updated_at_ms) VALUES (?, ?, ?) ON CONFLICT(setting_key) DO UPDATE SET value_json = excluded.value_json, updated_at_ms = excluded.updated_at_ms")
            .bind(IMAGE_SETTINGS_KEY).bind(serde_json::to_string(&settings)?).bind(now_ms())
            .execute(&self.pool).await?;
        Ok(settings)
    }
}
