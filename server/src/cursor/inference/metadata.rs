use crate::{
    cursor::protocol::proto::inference as pb,
    model::{ModelConfig, ModelType},
};

/// Prompt and tool decisions consumed by Cursor's local loop, before inference.
pub fn prompt_metadata(
    request: &pb::RunInferenceRunRequest,
    configured: Option<&ModelConfig>,
    models: &[ModelConfig],
) -> pb::RunInferencePromptModelMetadata {
    let selected = request
        .requested_model
        .as_ref()
        .expect("validated run model");
    let name = configured
        .map(|m| m.model_id.as_str())
        .unwrap_or(&selected.model_id);
    let slug = name.to_ascii_lowercase().replace('.', "-");
    let anthropic = configured.is_some_and(|m| m.model_type == ModelType::Anthropic);
    let prompt_version = request
        .agent_module
        .as_ref()
        .and_then(|m| {
            m.prompt_versions
                .iter()
                .find(|v| v.as_str() == "latest")
                .or_else(|| m.prompt_versions.first())
        })
        .cloned()
        .unwrap_or_else(|| "latest".into());
    let mut policies: Vec<_> = models
        .iter()
        .map(|m| pb::RunInferenceSubagentModelPolicy {
            slug: m.model_hash.clone(),
            advertised: true,
            valid: true,
            ..Default::default()
        })
        .collect();
    if !policies.iter().any(|m| m.slug == selected.model_id) {
        policies.push(pb::RunInferenceSubagentModelPolicy {
            slug: selected.model_id.clone(),
            advertised: true,
            valid: true,
            ..Default::default()
        });
    }
    let overrides = request
        .subagent_model_overrides
        .iter()
        .filter_map(|o| {
            use pb::run_inference_subagent_model_override::Selection;
            let (kind, model_id) = match &o.selection {
                Some(Selection::Model(m)) if m.model_id != "default" => {
                    ("model", Some(m.model_id.clone()))
                }
                Some(Selection::Model(_)) | Some(Selection::Inherit(true)) => ("inherit", None),
                Some(Selection::Disabled(true)) => ("disabled", None),
                _ => return None,
            };
            Some(pb::RunInferenceResolvedSubagentModelOverride {
                subagent_type: o.subagent_type.clone(),
                kind: kind.into(),
                model_id,
            })
        })
        .collect();
    pb::RunInferencePromptModelMetadata {
        vendor: if anthropic { "anthropic" } else { "openai" }.into(),
        prompt_version,
        is_claude_4x: anthropic
            && (slug.contains("claude-4")
                || slug.contains("sonnet-4")
                || slug.contains("opus-4")
                || slug.contains("haiku-4")),
        is_sonnet4: anthropic && slug.contains("sonnet-4"),
        is_sonnet45: anthropic && slug.contains("sonnet-4-5"),
        is_opus45: anthropic && slug.contains("opus-4-5"),
        is_opus46: anthropic && slug.contains("opus-4-6"),
        is_gpt5_family: slug.contains("gpt-5"),
        is_gpt5: slug.contains("gpt-5"),
        is_gpt51: slug.contains("gpt-5-1"),
        is_gpt52: slug.contains("gpt-5-2"),
        is_gpt53_codex: slug.contains("gpt-5-3") && slug.contains("codex"),
        is_gpt54: slug.contains("gpt-5-4"),
        is_gpt55: slug.contains("gpt-5-5"),
        is_gpt56: slug.contains("gpt-5-6"),
        is_codex_family: slug.contains("codex"),
        self_identity_name: "Cursor".into(),
        reasoning_effort: selected
            .parameters
            .iter()
            .find(|p| p.id == "effort" || p.id == "reasoning")
            .map(|p| p.value.clone())
            .or_else(|| {
                configured.and_then(|m| {
                    if anthropic {
                        m.anthropic_thinking_effort.clone()
                    } else {
                        m.reasoning_effort.clone()
                    }
                })
            }),
        agent_token_limit: configured
            .and_then(|m| m.context_window_tokens)
            .map(|n| n.min(i32::MAX as u64) as i32),
        // Cursor looks up every mode, including Plan while building Agent prompts.
        agent_mode_configs: ["agent", "chat", "plan", "debug", "triage", "multitask"]
            .into_iter()
            .map(|mode| pb::RunInferenceAgentModeConfig {
                mode_id: mode.into(),
                enable_ask_question_tool: true,
                ..Default::default()
            })
            .collect(),
        ask_question_config: Some(pb::RunInferenceAskQuestionConfig {
            tool_name: "AskQuestion".into(),
            auto_reject_first: false,
        }),
        subagent_model_config: Some(pb::RunInferenceSubagentModelConfig {
            force_policy: "none".into(),
            parent_max_mode: selected.max_mode,
            parent_requested_model_name: selected.model_id.clone(),
            models: policies,
            overrides,
            ..Default::default()
        }),
        ..Default::default()
    }
}
