//! Builds the Kiro control-plane model catalog from the model library.
//!
//! Kiro's model picker is driven entirely by the `ListAvailableModels`
//! response, so every configured model is published under its `model_hash`.
//! Kiro echoes that identifier back on the runtime request, which is how the
//! gateway resolves the model without a separate mapping setting.

use serde_json::{json, Value};

use crate::model::ModelConfig;

/// Input window assumed for a model that does not declare one.
const DEFAULT_CONTEXT_WINDOW: u64 = 200_000;
/// Output limit assumed for a model that does not declare one.
const DEFAULT_MAX_OUTPUT: u64 = 64_000;

/// Builds the `ListAvailableModels` response body.
pub fn build(models: &[ModelConfig]) -> Value {
    let entries: Vec<Value> = models.iter().map(entry).collect();
    let mut body = json!({ "models": entries });
    if let Some(first) = models.first() {
        body["defaultModel"] = json!({ "modelId": first.model_hash });
    }
    body
}

fn entry(model: &ModelConfig) -> Value {
    json!({
        "modelId": model.model_hash,
        "modelName": model.display_name,
        // The provider group is the most useful thing to show under the name.
        "description": model.group_name.clone().unwrap_or_default(),
        "rateUnit": "Credit",
        "supportedInputTypes": ["TEXT", "IMAGE"],
        // Kiro reads these for its caching panel. They describe the gateway,
        // not the upstream provider, so they are reported optimistically.
        "promptCaching": {
            "maximumCacheCheckpointsPerRequest": 4,
            "minimumTokensPerCacheCheckpoint": 1024,
            "supportsPromptCaching": true,
        },
        "tokenLimits": {
            "maxInputTokens": model.context_window_tokens.unwrap_or(DEFAULT_CONTEXT_WINDOW),
            "maxOutputTokens": model.max_output_tokens().unwrap_or(DEFAULT_MAX_OUTPUT),
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{ModelType, OPENAI_CHAT_ENDPOINT};

    fn model(hash: &str, name: &str) -> ModelConfig {
        ModelConfig {
            model_hash: hash.into(),
            sort_order: 0,
            display_name: name.into(),
            group_name: None,
            model_type: ModelType::OpenAi,
            base_url: "https://provider.example/v1/chat/completions".into(),
            use_full_url: true,
            api_key: "provider-secret".into(),
            tooltip_data: name.into(),
            model_id: "upstream-model".into(),
            reasoning_effort: None,
            openai_endpoint: OPENAI_CHAT_ENDPOINT.into(),
            openai_extra_params_enabled: false,
            openai_extra_params: json!({}),
            custom_headers_enabled: false,
            custom_headers: json!({}),
            anthropic_extra_params_enabled: false,
            anthropic_extra_params: json!({}),
            context_window_tokens: None,
            max_completion_tokens: None,
            anthropic_max_tokens: None,
            anthropic_thinking_effort: None,
            thinking_budget_tokens: None,
            created_at_ms: 0,
            updated_at_ms: 0,
        }
    }

    #[test]
    fn each_model_is_published_under_its_hash() {
        let body = build(&[model("aaaa1111", "First"), model("bbbb2222", "Second")]);
        let models = body["models"].as_array().unwrap();
        assert_eq!(models.len(), 2);
        assert_eq!(models[0]["modelId"], json!("aaaa1111"));
        assert_eq!(models[0]["modelName"], json!("First"));
        assert_eq!(models[1]["modelId"], json!("bbbb2222"));
    }

    #[test]
    fn the_first_model_is_the_default() {
        let body = build(&[model("aaaa1111", "First"), model("bbbb2222", "Second")]);
        assert_eq!(body["defaultModel"]["modelId"], json!("aaaa1111"));
    }

    #[test]
    fn an_empty_library_omits_the_default_model() {
        let body = build(&[]);
        assert_eq!(body["models"], json!([]));
        assert!(body.get("defaultModel").is_none());
    }

    #[test]
    fn declared_limits_win_over_the_fallbacks() {
        let mut configured = model("aaaa1111", "Configured");
        configured.context_window_tokens = Some(1_000_000);
        configured.max_completion_tokens = Some(32_000);

        let body = build(&[configured]);
        assert_eq!(body["models"][0]["tokenLimits"]["maxInputTokens"], json!(1_000_000));
        assert_eq!(body["models"][0]["tokenLimits"]["maxOutputTokens"], json!(32_000));

        let body = build(&[model("bbbb2222", "Undeclared")]);
        assert_eq!(
            body["models"][0]["tokenLimits"]["maxInputTokens"],
            json!(DEFAULT_CONTEXT_WINDOW)
        );
        assert_eq!(
            body["models"][0]["tokenLimits"]["maxOutputTokens"],
            json!(DEFAULT_MAX_OUTPUT)
        );
    }

    #[test]
    fn the_provider_group_is_used_as_the_description() {
        let mut grouped = model("aaaa1111", "Grouped");
        grouped.group_name = Some("My Provider".into());
        let body = build(&[grouped]);
        assert_eq!(body["models"][0]["description"], json!("My Provider"));
    }
}
