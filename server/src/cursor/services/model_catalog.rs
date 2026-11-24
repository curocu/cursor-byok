//! Publishes the configured model catalog to Cursor.
use axum::{
    body::{Body, Bytes},
    extract::{Extension, State},
    http::{header, HeaderValue, Request, Response, StatusCode},
};
use bytes::{BufMut, BytesMut};
use prost::Message;

use crate::{
    api::cursor::proxy::{self, CursorProxy},
    cursor::{protocol::proto::agent::v1 as agent, transport::TransportRegistry},
    model::{format_token_count, parse_token_count, ModelConfig},
    Error, Result,
};

#[derive(Clone, PartialEq, Message)]
struct AvailableModelsAddition {
    #[prost(string, repeated, tag = "1")]
    model_names: Vec<String>,
    #[prost(message, repeated, tag = "2")]
    models: Vec<AvailableModel>,
    #[prost(message, optional, tag = "15")]
    display_configuration: Option<ModelPickerDisplayConfiguration>,
}

#[derive(Clone, PartialEq, Message)]
struct ModelPickerDisplayConfiguration {
    #[prost(message, optional, tag = "1")]
    routed_model_view_config: Option<RoutedModelViewConfig>,
    #[prost(message, optional, tag = "2")]
    named_models_view_config: Option<NamedModelsViewConfig>,
}

#[derive(Clone, PartialEq, Message)]
struct RoutedModelViewConfig {
    #[prost(bool, optional, tag = "5")]
    hide_routed_model_view: Option<bool>,
}

#[derive(Clone, PartialEq, Message)]
struct NamedModelsViewConfig {
    #[prost(message, optional, tag = "3")]
    named_view_to_routed_model_view_no_button: Option<NoRoutedModelButton>,
}

#[derive(Clone, PartialEq, Message)]
struct NoRoutedModelButton {}

#[derive(Clone, PartialEq, Message)]
struct AvailableModel {
    #[prost(string, tag = "1")]
    name: String,
    #[prost(bool, tag = "2")]
    default_on: bool,
    #[prost(bool, optional, tag = "5")]
    supports_agent: Option<bool>,
    #[prost(int32, optional, tag = "6")]
    degradation_status: Option<i32>,
    #[prost(message, optional, tag = "8")]
    tooltip_data: Option<TooltipData>,
    #[prost(bool, optional, tag = "9")]
    supports_thinking: Option<bool>,
    #[prost(bool, optional, tag = "10")]
    supports_images: Option<bool>,
    #[prost(bool, optional, tag = "14")]
    supports_max_mode: Option<bool>,
    #[prost(string, optional, tag = "17")]
    client_display_name: Option<String>,
    #[prost(string, optional, tag = "18")]
    server_model_name: Option<String>,
    #[prost(bool, optional, tag = "19")]
    supports_non_max_mode: Option<bool>,
    #[prost(message, optional, tag = "20")]
    tooltip_data_for_max_mode: Option<TooltipData>,
    #[prost(bool, optional, tag = "21")]
    is_recommended_for_background_composer: Option<bool>,
    #[prost(bool, optional, tag = "22")]
    supports_plan_mode: Option<bool>,
    #[prost(string, optional, tag = "24")]
    inputbox_short_model_name: Option<String>,
    #[prost(bool, optional, tag = "25")]
    supports_sandboxing: Option<bool>,
    #[prost(bool, optional, tag = "26")]
    supports_cmd_k: Option<bool>,
    #[prost(message, repeated, tag = "29")]
    parameter_definitions: Vec<ModelParameterDefinition>,
    #[prost(message, repeated, tag = "30")]
    variants: Vec<ModelVariant>,
    #[prost(string, repeated, tag = "36")]
    legacy_slugs: Vec<String>,
    #[prost(int32, optional, tag = "38")]
    named_model_section_index: Option<i32>,
    #[prost(string, optional, tag = "41")]
    vendor_name: Option<String>,
    #[prost(message, optional, tag = "42")]
    vendor: Option<AvailableModelVendor>,
    #[prost(message, repeated, tag = "48")]
    model_picker_badges: Vec<ModelPickerBadge>,
}

#[derive(Clone, PartialEq, Message)]
struct TooltipData {
    #[prost(string, optional, tag = "7")]
    markdown_content: Option<String>,
}

#[derive(Clone, PartialEq, Message)]
struct ModelParameterDefinition {
    #[prost(string, tag = "1")]
    id: String,
    #[prost(string, tag = "2")]
    name: String,
    #[prost(string, optional, tag = "3")]
    markdown_tooltip: Option<String>,
    #[prost(message, optional, tag = "4")]
    parameter_type: Option<ModelParameterType>,
    #[prost(bool, optional, tag = "5")]
    is_cycleable_by_hotkey: Option<bool>,
}

#[derive(Clone, PartialEq, Message)]
struct ModelParameterType {
    #[prost(message, optional, tag = "1")]
    boolean_parameter: Option<BooleanParameter>,
    #[prost(message, optional, tag = "2")]
    enum_parameter: Option<EnumParameter>,
}

#[derive(Clone, PartialEq, Message)]
struct BooleanParameter {
    #[prost(message, repeated, tag = "1")]
    values: Vec<BooleanParameterValue>,
}

#[derive(Clone, PartialEq, Message)]
struct BooleanParameterValue {
    #[prost(string, tag = "1")]
    value: String,
    #[prost(string, optional, tag = "2")]
    display_name: Option<String>,
    #[prost(bool, optional, tag = "3")]
    increases_model_cost: Option<bool>,
}

#[derive(Clone, PartialEq, Message)]
struct EnumParameter {
    #[prost(message, repeated, tag = "1")]
    values: Vec<EnumParameterValue>,
}

#[derive(Clone, PartialEq, Message)]
struct EnumParameterValue {
    #[prost(string, tag = "1")]
    value: String,
    #[prost(string, optional, tag = "2")]
    display_name: Option<String>,
}

#[derive(Clone, PartialEq, Message)]
struct ModelVariant {
    #[prost(message, repeated, tag = "1")]
    parameter_values: Vec<ModelParameterValue>,
    #[prost(string, tag = "2")]
    display_name: String,
    #[prost(bool, tag = "3")]
    is_max_mode: bool,
    #[prost(bool, optional, tag = "4")]
    is_default_max_config: Option<bool>,
    #[prost(bool, optional, tag = "5")]
    is_default_non_max_config: Option<bool>,
    #[prost(message, optional, tag = "6")]
    tooltip_data: Option<TooltipData>,
    #[prost(string, optional, tag = "8")]
    display_name_outside_picker: Option<String>,
    #[prost(string, optional, tag = "9")]
    variant_string_representation: Option<String>,
    #[prost(string, optional, tag = "11")]
    legacy_slug: Option<String>,
}

#[derive(Clone, PartialEq, Message)]
struct ModelParameterValue {
    #[prost(string, tag = "1")]
    id: String,
    #[prost(string, tag = "2")]
    value: String,
}

#[derive(Clone, PartialEq, Message)]
struct ModelPickerBadge {
    #[prost(string, tag = "1")]
    label: String,
    #[prost(int32, tag = "2")]
    variant: i32,
    #[prost(bool, tag = "3")]
    dismiss_on_selection: bool,
}

#[derive(Clone, PartialEq, Message)]
struct AvailableModelVendor {
    #[prost(int32, tag = "1")]
    id: i32,
    #[prost(string, tag = "2")]
    display_name: String,
}

#[derive(Clone, PartialEq, Message)]
struct UsableModelsAddition {
    #[prost(message, repeated, tag = "1")]
    models: Vec<agent::ModelDetails>,
}

#[derive(Clone, PartialEq, Message)]
struct DefaultModelResponse {
    #[prost(string, tag = "1")]
    model: String,
    #[prost(string, tag = "2")]
    thinking_model: String,
    #[prost(bool, tag = "3")]
    max_mode: bool,
    #[prost(string, tag = "4")]
    next_default_set_date: String,
}

#[derive(Clone, PartialEq, Message)]
struct DefaultModelNudgeDataResponse {
    #[prost(string, tag = "1")]
    nudge_date: String,
    #[prost(bool, tag = "2")]
    should_default_switch_on_new_chat: bool,
    #[prost(string, repeated, tag = "3")]
    models_with_no_default_switch: Vec<String>,
    #[prost(string, tag = "4")]
    conversion_model_override: String,
}

const CLI_LOCAL_MODEL_API_KEY: &str = "cursor-byok-local";

const CONTEXTS: [(&str, &str); 5] = [
    ("200k", "200K"),
    ("356k", "356K"),
    ("500k", "500K"),
    ("800k", "800K"),
    ("1m", "1M"),
];
const EFFORTS: [(&str, &str); 6] = [
    ("default", "Default"),
    ("low", "Low"),
    ("medium", "Medium"),
    ("high", "High"),
    ("xhigh", "Extra High"),
    ("max", "Max"),
];
const DEFAULT_CONTEXT: &str = "200k";

fn context_options(context_window_tokens: Option<u64>) -> Vec<(String, String)> {
    let mut contexts = CONTEXTS
        .into_iter()
        .map(|(value, display_name)| (value.to_owned(), display_name.to_owned()))
        .collect::<Vec<_>>();
    if let Some(tokens) = context_window_tokens {
        let value = tokens.to_string();
        let duplicate = contexts
            .iter()
            .any(|(existing, _)| parse_token_count(existing) == Some(tokens));
        if !duplicate {
            contexts.push((value, format!("{} (Custom)", format_token_count(tokens))));
        }
    }
    contexts
}

pub async fn available_models(
    State(registry): State<TransportRegistry>,
    Extension(proxy): Extension<CursorProxy>,
    request: Request<Body>,
) -> Result<Response<Body>> {
    let models = registry.store().chat_models().await?;
    tracing::info!(
        model_count = models.len(),
        "publishing BYOK-only models to Cursor AvailableModels"
    );
    let local = available_catalog(&models).encode_to_vec();
    match proxy::forward_buffered(&proxy, request).await {
        Ok(upstream) => replace_response(upstream, local, &[1, 2, 15, 19, 20, 21]),
        Err(error) => {
            tracing::warn!(%error, "Cursor AvailableModels upstream unavailable; using local catalog");
            Ok(local_response(local))
        }
    }
}

fn available_catalog(models: &[ModelConfig]) -> AvailableModelsAddition {
    AvailableModelsAddition {
        model_names: models
            .iter()
            .map(|model| model.model_hash.clone())
            .collect(),
        models: models.iter().map(available_model).collect(),
        display_configuration: Some(ModelPickerDisplayConfiguration {
            routed_model_view_config: Some(RoutedModelViewConfig {
                hide_routed_model_view: Some(true),
            }),
            // Cursor normalizes this marker into explicit nulls that clear
            // cached Auto toggles instead of retaining omitted properties.
            named_models_view_config: Some(NamedModelsViewConfig {
                named_view_to_routed_model_view_no_button: Some(NoRoutedModelButton {}),
            }),
        }),
    }
}

pub async fn usable_models(
    State(registry): State<TransportRegistry>,
    Extension(proxy): Extension<CursorProxy>,
    request: Request<Body>,
) -> Result<Response<Body>> {
    let models = registry.store().chat_models().await?;
    tracing::info!(
        model_count = models.len(),
        "publishing BYOK-only models to Cursor GetUsableModels"
    );
    let local = UsableModelsAddition {
        models: models.iter().map(usable_model).collect(),
    }
    .encode_to_vec();
    match proxy::forward_buffered(&proxy, request).await {
        Ok(upstream) => replace_response(upstream, local, &[1]),
        Err(error) => {
            tracing::warn!(%error, "Cursor GetUsableModels upstream unavailable; using local catalog");
            Ok(local_response(local))
        }
    }
}

pub async fn default_model_for_cli(
    State(registry): State<TransportRegistry>,
) -> Result<Response<Body>> {
    let models = registry.store().chat_models().await?;
    Ok(local_response(
        agent::GetDefaultModelForCliResponse {
            model: default_model_details(&models),
        }
        .encode_to_vec(),
    ))
}

pub async fn default_model(State(registry): State<TransportRegistry>) -> Result<Response<Body>> {
    let models = registry.store().chat_models().await?;
    Ok(local_response(
        default_model_response(&models).encode_to_vec(),
    ))
}

pub async fn default_model_nudge(
    State(registry): State<TransportRegistry>,
) -> Result<Response<Body>> {
    let models = registry.store().chat_models().await?;
    Ok(local_response(
        default_model_nudge_response(&models).encode_to_vec(),
    ))
}

fn default_model_details(models: &[ModelConfig]) -> Option<agent::ModelDetails> {
    models.first().map(usable_model)
}

fn default_model_id<'a>(models: &'a [ModelConfig]) -> &'a str {
    models
        .first()
        .map(|model| model.model_hash.as_str())
        .unwrap_or_default()
}

fn default_model_response(models: &[ModelConfig]) -> DefaultModelResponse {
    let model = default_model_id(models).to_owned();
    DefaultModelResponse {
        thinking_model: model.clone(),
        model,
        max_mode: false,
        next_default_set_date: String::new(),
    }
}

fn default_model_nudge_response(models: &[ModelConfig]) -> DefaultModelNudgeDataResponse {
    DefaultModelNudgeDataResponse {
        nudge_date: "0".into(),
        should_default_switch_on_new_chat: false,
        models_with_no_default_switch: models
            .iter()
            .map(|model| model.model_hash.clone())
            .collect(),
        conversion_model_override: String::new(),
    }
}

fn replace_response(
    upstream: proxy::BufferedResponse,
    local: Vec<u8>,
    replaced_fields: &[u32],
) -> Result<Response<Body>> {
    if !upstream.status.is_success() {
        tracing::warn!(status = %upstream.status, "Cursor model catalog upstream rejected request; using local catalog");
        return Ok(local_response(local));
    }
    let (framed, mut payload) = unary_payload(&upstream.body)?;
    // Replace the catalog rather than concatenating repeated model fields.
    // Keep unknown metadata intact so Cursor retains its other configuration.
    let mut catalog = BytesMut::with_capacity(payload.len() + local.len());
    while !payload.is_empty() {
        let field = payload;
        let (tag, wire_type) = prost::encoding::decode_key(&mut payload)?;
        prost::encoding::skip_field(wire_type, tag, &mut payload, Default::default())?;
        if !replaced_fields.contains(&tag) {
            catalog.extend_from_slice(&field[..field.len() - payload.len()]);
        }
    }
    catalog.extend_from_slice(&local);
    let body = if framed {
        let mut body = BytesMut::with_capacity(5 + catalog.len());
        body.put_u8(0);
        body.put_u32(catalog.len() as u32);
        body.extend_from_slice(&catalog);
        body.freeze()
    } else {
        catalog.freeze()
    };
    Ok(upstream.with_body(body))
}

fn local_response(body: Vec<u8>) -> Response<Body> {
    let mut response = Response::new(Body::from(body));
    *response.status_mut() = StatusCode::OK;
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/proto"),
    );
    response
}

fn unary_payload(body: &Bytes) -> Result<(bool, &[u8])> {
    if body.len() < 5 {
        return Ok((false, body));
    }
    let flags = body[0];
    let length = u32::from_be_bytes([body[1], body[2], body[3], body[4]]) as usize;
    if length != body.len() - 5 {
        return Ok((false, body));
    }
    if flags != 0 {
        return Err(Error::Protocol(format!(
            "invalid Cursor model catalog frame: flags={flags}"
        )));
    }
    Ok((true, &body[5..]))
}

fn available_model(model: &ModelConfig) -> AvailableModel {
    let contexts = context_options(model.context_window_tokens);
    let tooltip = model_tooltip(model);
    let variants = model_variants(model, &tooltip, &contexts, true);
    let legacy_slugs = variants
        .iter()
        .filter_map(|variant| variant.legacy_slug.clone())
        .collect();
    AvailableModel {
        name: model.model_hash.clone(),
        default_on: true,
        supports_agent: Some(true),
        degradation_status: Some(0),
        tooltip_data: Some(tooltip.clone()),
        supports_thinking: Some(true),
        supports_images: Some(true),
        supports_max_mode: Some(true),
        client_display_name: Some(model.display_name.clone()),
        server_model_name: Some(model.model_hash.clone()),
        supports_non_max_mode: Some(true),
        tooltip_data_for_max_mode: Some(tooltip),
        is_recommended_for_background_composer: Some(false),
        supports_plan_mode: Some(true),
        inputbox_short_model_name: Some(model.display_name.clone()),
        supports_sandboxing: Some(true),
        supports_cmd_k: Some(false),
        parameter_definitions: model_parameters(&contexts, true),
        variants,
        legacy_slugs,
        named_model_section_index: Some(1),
        vendor_name: Some("cursor".into()),
        vendor: Some(AvailableModelVendor {
            id: 6,
            display_name: "Cursor".into(),
        }),
        model_picker_badges: vec![ModelPickerBadge {
            label: model
                .group_name
                .clone()
                .unwrap_or_else(|| provider_host(&model.base_url)),
            variant: 1,
            dismiss_on_selection: false,
        }],
    }
}

/// 徽章回退标签:base_url 的主机名。入库时已校验为带主机的 HTTP(S) URL,
/// 解析失败仅是理论分支,此时原样返回 base_url。
fn provider_host(base_url: &str) -> String {
    reqwest::Url::parse(base_url.trim())
        .ok()
        .and_then(|url| url.host_str().map(str::to_lowercase))
        .unwrap_or_else(|| base_url.trim().into())
}

fn model_parameters(
    contexts: &[(String, String)],
    thinking: bool,
) -> Vec<ModelParameterDefinition> {
    let mut parameters = vec![ModelParameterDefinition {
        id: "context".into(),
        name: "Context".into(),
        markdown_tooltip: Some("Context size used to trigger conversation compaction.".into()),
        parameter_type: Some(ModelParameterType {
            boolean_parameter: None,
            enum_parameter: Some(EnumParameter {
                values: contexts
                    .iter()
                    .map(|(value, display_name)| EnumParameterValue {
                        value: value.clone(),
                        display_name: Some(display_name.clone()),
                    })
                    .collect(),
            }),
        }),
        is_cycleable_by_hotkey: Some(false),
    }];
    if thinking {
        parameters.push(ModelParameterDefinition {
            id: "reasoning".into(),
            name: "Effort".into(),
            markdown_tooltip: Some("Effort the model uses to generate its response.".into()),
            parameter_type: Some(ModelParameterType {
                boolean_parameter: None,
                enum_parameter: Some(EnumParameter {
                    values: EFFORTS
                        .into_iter()
                        .map(|(value, display_name)| EnumParameterValue {
                            value: value.into(),
                            display_name: Some(display_name.into()),
                        })
                        .collect(),
                }),
            }),
            is_cycleable_by_hotkey: Some(true),
        });
    }
    parameters.push(ModelParameterDefinition {
        id: "fast".into(),
        name: "Fast".into(),
        markdown_tooltip: Some("Significantly faster but consumes more usage".into()),
        parameter_type: Some(ModelParameterType {
            boolean_parameter: Some(BooleanParameter {
                values: vec![
                    BooleanParameterValue {
                        value: "false".into(),
                        display_name: None,
                        increases_model_cost: None,
                    },
                    BooleanParameterValue {
                        value: "true".into(),
                        display_name: Some("Fast".into()),
                        increases_model_cost: Some(true),
                    },
                ],
            }),
            enum_parameter: None,
        }),
        is_cycleable_by_hotkey: Some(false),
    });
    parameters
}

fn model_variants(
    model: &ModelConfig,
    tooltip: &TooltipData,
    contexts: &[(String, String)],
    thinking: bool,
) -> Vec<ModelVariant> {
    let efforts: Vec<_> = if thinking {
        EFFORTS.into_iter().map(Some).collect()
    } else {
        vec![None]
    };
    let mut defaults = crate::model::ModelSpec::new(&model.model_hash);
    model.configure(&mut defaults);
    let default_context = defaults.context_window_tokens.unwrap_or(200_000);
    let default_effort = defaults.reasoning.effort.as_deref().unwrap_or("default");
    let mut variants = Vec::with_capacity(contexts.len() * efforts.len() * 2);
    for (context, context_name) in contexts {
        for effort in &efforts {
            for fast in [false, true] {
                let mut variant = model_variant(
                    &model.model_hash,
                    &model.display_name,
                    tooltip,
                    context,
                    context_name,
                    *effort,
                    fast,
                );
                let is_default = parse_token_count(context) == Some(default_context)
                    && !fast
                    && effort.is_none_or(|(value, _)| value == default_effort);
                variant.is_default_max_config = is_default.then_some(true);
                variant.is_default_non_max_config = is_default.then_some(true);
                variants.push(variant);
            }
        }
    }
    variants
}

fn model_variant(
    name: &str,
    display_name: &str,
    tooltip: &TooltipData,
    context: &str,
    context_name: &str,
    effort: Option<(&str, &str)>,
    fast: bool,
) -> ModelVariant {
    let mut suffix = Vec::with_capacity(3);
    if context != DEFAULT_CONTEXT {
        suffix.push(context_name);
    }
    if let Some((_, effort_name)) = effort {
        suffix.push(effort_name);
    }
    if fast {
        suffix.push("Fast");
    }
    let suffix = suffix.join(" ");
    let display_name = if suffix.is_empty() {
        display_name.to_owned()
    } else {
        format!(
            "{display_name} <span style=\"color: var(--cursor-text-tertiary);\">{suffix}</span>"
        )
    };
    let mut parameter_values = vec![ModelParameterValue {
        id: "context".into(),
        value: context.into(),
    }];
    if let Some((effort, _)) = effort {
        parameter_values.push(ModelParameterValue {
            id: "reasoning".into(),
            value: effort.into(),
        });
    }
    parameter_values.push(ModelParameterValue {
        id: "fast".into(),
        value: fast.to_string(),
    });
    ModelVariant {
        parameter_values,
        display_name: display_name.clone(),
        is_max_mode: false,
        is_default_max_config: None,
        is_default_non_max_config: None,
        tooltip_data: Some(tooltip.clone()),
        display_name_outside_picker: Some(display_name),
        variant_string_representation: Some(match effort {
            Some((effort, _)) => {
                format!("{name}[context={context},reasoning={effort},fast={fast}]")
            }
            None => format!("{name}[context={context},fast={fast}]"),
        }),
        legacy_slug: Some(format!(
            "{name}-{context}{}{}",
            effort
                .map(|(effort, _)| format!("-{effort}"))
                .unwrap_or_default(),
            if fast { "-fast" } else { "" }
        )),
    }
}

fn model_tooltip(model: &ModelConfig) -> TooltipData {
    TooltipData {
        markdown_content: Some(model.tooltip_data.clone()),
    }
}

fn cli_local_model_credentials() -> agent::model_details::Credentials {
    agent::model_details::Credentials::ApiKeyCredentials(agent::ApiKeyCredentials {
        api_key: CLI_LOCAL_MODEL_API_KEY.into(),
        base_url: None,
    })
}

fn usable_model(model: &ModelConfig) -> agent::ModelDetails {
    agent::ModelDetails {
        model_id: model.model_hash.clone(),
        display_model_id: model.model_hash.clone(),
        display_name: model.display_name.clone(),
        display_name_short: model.display_name.clone(),
        thinking_details: Some(agent::ThinkingDetails::default()),
        credentials: Some(cli_local_model_credentials()),
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{ModelType, OPENAI_CHAT_ENDPOINT};

    fn model() -> ModelConfig {
        ModelConfig {
            model_hash: "local-model-hash".into(),
            sort_order: 0,
            display_name: "Local Model".into(),
            group_name: None,
            model_type: ModelType::OpenAi,
            base_url: "https://provider.example/v1/chat/completions".into(),
            use_full_url: true,
            api_key: "provider-secret".into(),
            tooltip_data: "Local Model".into(),
            model_id: "upstream-model".into(),
            reasoning_effort: None,
            openai_endpoint: OPENAI_CHAT_ENDPOINT.into(),
            openai_extra_params_enabled: false,
            openai_extra_params: serde_json::json!({}),
            custom_headers_enabled: false,
            custom_headers: serde_json::json!({}),
            anthropic_extra_params_enabled: false,
            anthropic_extra_params: serde_json::json!({}),
            context_window_tokens: None,
            max_completion_tokens: None,
            anthropic_max_tokens: None,
            anthropic_thinking_effort: None,
            thinking_budget_tokens: None,
            created_at_ms: 0,
            updated_at_ms: 0,
        }
    }

    fn default_selection(
        model: &ModelConfig,
    ) -> crate::cursor::protocol::proto::inference::InferenceRequestedModel {
        use crate::cursor::protocol::proto::inference as pb;
        let catalog = available_model(model);
        let catalog = AvailableModel::decode(catalog.encode_to_vec().as_slice()).unwrap();
        let defaults: Vec<_> = catalog
            .variants
            .iter()
            .filter(|variant| variant.is_default_non_max_config == Some(true))
            .collect();
        assert_eq!(defaults.len(), 1, "each model needs exactly one default");
        assert_eq!(defaults[0].is_default_max_config, Some(true));
        for parameter in &defaults[0].parameter_values {
            let definition = catalog
                .parameter_definitions
                .iter()
                .find(|definition| definition.id == parameter.id)
                .unwrap();
            let kind = definition.parameter_type.as_ref().unwrap();
            assert!(
                kind.enum_parameter.as_ref().is_some_and(|values| values
                    .values
                    .iter()
                    .any(|value| value.value == parameter.value))
                    || kind.boolean_parameter.as_ref().is_some_and(|values| values
                        .values
                        .iter()
                        .any(|value| value.value == parameter.value))
            );
        }
        pb::InferenceRequestedModel {
            model_id: model.model_hash.clone(),
            parameters: defaults[0]
                .parameter_values
                .iter()
                .map(|parameter| pb::InferenceModelParameterValue {
                    id: parameter.id.clone(),
                    value: parameter.value.clone(),
                })
                .collect(),
            ..Default::default()
        }
    }

    #[test]
    fn catalog_default_selection_preserves_saved_context_and_effort() {
        use crate::cursor::{
            inference::{invocation, prompt_metadata},
            protocol::proto::inference as pb,
        };
        for (model_type, endpoint) in [
            (ModelType::OpenAi, OPENAI_CHAT_ENDPOINT),
            (ModelType::OpenAi, crate::model::OPENAI_RESPONSES_ENDPOINT),
            (ModelType::Anthropic, ""),
        ] {
            for context in [128_000, 200_000, 1_000_000] {
                let mut configured = model();
                configured.model_type = model_type;
                configured.openai_endpoint = endpoint.into();
                configured.context_window_tokens = Some(context);
                configured.reasoning_effort = Some("low".into());
                configured.anthropic_thinking_effort = Some("medium".into());
                let selected = default_selection(&configured);
                let metadata = prompt_metadata(
                    &pb::RunInferenceRunRequest {
                        requested_model: Some(selected.clone()),
                        ..Default::default()
                    },
                    Some(&configured),
                    std::slice::from_ref(&configured),
                );
                let mut input = invocation(
                    &pb::InferenceStreamRequest {
                        requested_model: Some(selected),
                        ..Default::default()
                    },
                    &configured.model_hash,
                    "conversation",
                    "run",
                    "call",
                    0,
                )
                .unwrap();
                configured.configure(&mut input.request.model);
                let effort = if model_type == ModelType::Anthropic {
                    "medium"
                } else {
                    "low"
                };
                assert_eq!(metadata.agent_token_limit, Some(context as i32));
                assert_eq!(metadata.reasoning_effort.as_deref(), Some(effort));
                assert_eq!(input.request.model.context_window_tokens, Some(context));
                assert_eq!(
                    input.request.model.reasoning.effort.as_deref(),
                    Some(effort)
                );
            }
        }
    }

    #[test]
    fn catalog_default_does_not_force_reasoning_when_unconfigured() {
        use crate::cursor::{
            inference::{invocation, prompt_metadata},
            protocol::proto::inference as pb,
        };
        let configured = model();
        let selected = default_selection(&configured);
        let metadata = prompt_metadata(
            &pb::RunInferenceRunRequest {
                requested_model: Some(selected.clone()),
                ..Default::default()
            },
            Some(&configured),
            std::slice::from_ref(&configured),
        );
        let mut input = invocation(
            &pb::InferenceStreamRequest {
                requested_model: Some(selected),
                ..Default::default()
            },
            &configured.model_hash,
            "conversation",
            "run",
            "call",
            0,
        )
        .unwrap();
        configured.configure(&mut input.request.model);
        assert_eq!(metadata.agent_token_limit, Some(200_000));
        assert!(metadata.reasoning_effort.is_none());
        assert!(input.request.model.reasoning.effort.is_none());
        assert!(!input.request.model.reasoning.enabled);
        let catalog = available_model(&configured);
        assert!(catalog
            .parameter_definitions
            .iter()
            .any(|parameter| parameter.id == "reasoning"));
        assert!(catalog.variants.iter().any(|variant| variant
            .parameter_values
            .iter()
            .any(|parameter| parameter.id == "reasoning" && parameter.value == "high")));
    }

    #[tokio::test]
    async fn catalog_choices_reach_all_three_provider_requests() {
        use crate::{
            config::{ProviderConfig, ProviderKind},
            cursor::{inference::invocation, protocol::proto::inference as pb},
            provider::build_provider,
        };
        use axum::{routing::post, Json, Router};
        use futures_util::StreamExt;
        use serde_json::Value;
        use tokio_util::sync::CancellationToken;

        for (model_type, endpoint, kind, response) in [
            (ModelType::OpenAi, OPENAI_CHAT_ENDPOINT, ProviderKind::OpenAiChat,
                "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n"),
            (ModelType::OpenAi, crate::model::OPENAI_RESPONSES_ENDPOINT, ProviderKind::OpenAiResponses,
                "data: {\"type\":\"response.completed\",\"response\":{}}\n\n"),
            (ModelType::Anthropic, "", ProviderKind::Anthropic,
                "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"}}\n\ndata: {\"type\":\"message_stop\"}\n\n"),
        ] {
            for (saved, selection, expected) in [
                (Some("low"), None, Some("low")),
                (None, None, None),
                (Some("low"), Some("high"), Some("high")),
                (Some("low"), Some("default"), Some("low")),
            ] {
                let mut configured = model();
                configured.model_type = model_type;
                configured.openai_endpoint = endpoint.into();
                configured.context_window_tokens = Some(128_000);
                configured.reasoning_effort = saved.map(str::to_owned);
                configured.anthropic_thinking_effort = saved.map(str::to_owned);
                let mut selected = default_selection(&configured);
                if let Some(selection) = selection {
                    selected.parameters.iter_mut().find(|p| p.id == "reasoning").unwrap().value = selection.into();
                }
                let mut input = invocation(&pb::InferenceStreamRequest {
                    requested_model: Some(selected), ..Default::default()
                }, &configured.model_hash, "conversation", "run", "call", 0).unwrap();
                configured.configure(&mut input.request.model);
                assert_eq!(input.request.model.context_window_tokens, Some(128_000));
                let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
                let app = Router::new().route("/", post(move |Json(body): Json<Value>| {
                    let sender = sender.clone();
                    async move {
                        sender.send(body).unwrap();
                        ([("content-type", "text/event-stream")], response)
                    }
                }));
                let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
                let address = listener.local_addr().unwrap();
                let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap(); });
                let provider = build_provider(&ProviderConfig {
                    kind: kind.clone(),
                    request_url: format!("http://{address}/"),
                    api_key: "test-key".into(),
                    custom_headers: Default::default(),
                    max_output_tokens: Some(2048),
                    request_timeout: std::time::Duration::from_secs(5),
                    allowed_body_fields: None,
                }).unwrap();
                let mut stream = provider.stream(input, CancellationToken::new());
                while let Some(event) = stream.next().await { event.unwrap(); }
                let body = receiver.recv().await.unwrap();
                server.abort();
                let actual = match kind {
                    ProviderKind::OpenAiChat => body["reasoning_effort"].as_str(),
                    ProviderKind::OpenAiResponses => body["reasoning"]["effort"].as_str(),
                    ProviderKind::Anthropic => body["output_config"]["effort"].as_str(),
                };
                assert_eq!(actual, expected, "{kind:?}: saved={saved:?}, selection={selection:?}");
                if expected.is_none() {
                    assert!(body.get("reasoning_effort").is_none());
                    assert!(body.get("reasoning").is_none());
                    assert!(body.get("thinking").is_none());
                    assert!(body.get("output_config").is_none());
                }
            }
        }
    }

    #[tokio::test]
    async fn default_effort_call_record_matches_routed_request() {
        use crate::{
            cursor::{inference::invocation, protocol::proto::inference as pb},
            model::ModelConfigInput,
            network::NetworkClients,
            provider::{Provider, ProviderRouter},
            store::Store,
        };
        use axum::{routing::post, Json, Router};
        use futures_util::StreamExt;
        use serde_json::{json, Value};
        use tokio_util::sync::CancellationToken;
        let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
        let app = Router::new().route(
            "/",
            post(move |Json(body): Json<Value>| {
                let sender = sender.clone();
                async move {
                    sender.send(body).unwrap();
                    (
                        [("content-type", "text/event-stream")],
                        "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
                    )
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let dir = tempfile::tempdir().unwrap();
        let store = Store::connect(&format!(
            "sqlite://{}",
            dir.path().join("test.db").display()
        ))
        .await
        .unwrap();
        let input: ModelConfigInput = serde_json::from_value(json!({
            "type":"openai", "display_name":"Test", "base_url":format!("http://{address}/"),
            "use_full_url":true, "api_key":"test-key", "tooltip_data":"Test",
            "model_id":"test-model", "reasoning_effort":"low", "openai_endpoint":OPENAI_CHAT_ENDPOINT,
        })).unwrap();
        let configured = store.create_model(&input).await.unwrap();
        let mut selected = default_selection(&configured);
        selected
            .parameters
            .iter_mut()
            .find(|p| p.id == "reasoning")
            .unwrap()
            .value = "default".into();
        let input = invocation(
            &pb::InferenceStreamRequest {
                requested_model: Some(selected),
                ..Default::default()
            },
            &configured.model_hash,
            "conversation",
            "run",
            "call",
            0,
        )
        .unwrap();
        let timeout = std::time::Duration::from_secs(5);
        let provider = ProviderRouter::new(
            store.clone(),
            NetworkClients::new(store.clone()),
            timeout,
            timeout,
        );
        let mut stream = provider.stream(input, CancellationToken::new());
        while let Some(event) = stream.next().await {
            event.unwrap();
        }
        let body = receiver.recv().await.unwrap();
        server.abort();
        assert_eq!(body["reasoning_effort"], "low");
        let call = store.llm_call("run:call").await.unwrap().unwrap();
        assert_eq!(call.reasoning_effort.as_deref(), Some("low"));
    }

    #[derive(Clone, PartialEq, Message)]
    struct CatalogMetadata {
        #[prost(bool, tag = "11")]
        use_model_parameters: bool,
        #[prost(string, tag = "88")]
        unrelated_metadata: String,
        #[prost(bytes, tag = "15")]
        display_configuration: Vec<u8>,
        #[prost(string, optional, tag = "19")]
        experimental_model_id: Option<String>,
        #[prost(string, optional, tag = "21")]
        nudge_new_chats_to_auto_optimize_for: Option<String>,
    }

    #[derive(Clone, PartialEq, Message)]
    struct PickerConfigurationFixture {
        #[prost(message, optional, tag = "1")]
        routed_model_view_config: Option<RoutedModelViewConfig>,
        #[prost(bytes, tag = "2")]
        named_models_view_config: Vec<u8>,
    }

    #[derive(Clone, PartialEq, Message)]
    struct NamedPickerConfigurationFixture {
        #[prost(bytes, optional, tag = "2")]
        auto_toggle: Option<Vec<u8>>,
        #[prost(bytes, optional, tag = "3")]
        no_button: Option<Vec<u8>>,
    }

    fn upstream_catalog(payload: &[u8], framed: bool) -> proxy::BufferedResponse {
        let body = if framed {
            let mut body = BytesMut::new();
            body.put_u8(0);
            body.put_u32(payload.len() as u32);
            body.extend_from_slice(payload);
            body.freeze()
        } else {
            Bytes::copy_from_slice(payload)
        };
        proxy::BufferedResponse {
            status: StatusCode::OK,
            headers: Default::default(),
            body,
        }
    }

    #[tokio::test]
    async fn available_catalog_refresh_replaces_official_models_and_preserves_metadata() {
        let local = available_catalog(&[model()]).encode_to_vec();
        let mut official = model();
        official.model_hash = "official-model".into();
        let mut upstream = AvailableModelsAddition {
            model_names: vec![official.model_hash.clone()],
            models: vec![available_model(&official)],
            ..Default::default()
        }
        .encode_to_vec();
        let metadata = CatalogMetadata {
            use_model_parameters: true,
            unrelated_metadata: "preserve-future-cursor-fields".into(),
            display_configuration: PickerConfigurationFixture {
                routed_model_view_config: None,
                // NamedModelsViewConfig.named_view_to_routed_model_view_toggle.
                named_models_view_config: vec![0x12, 0],
            }
            .encode_to_vec(),
            experimental_model_id: Some("official-experimental-model".into()),
            nudge_new_chats_to_auto_optimize_for: Some("speed".into()),
        };
        upstream.extend(metadata.encode_to_vec());
        for framed in [false, true] {
            let mut payload = upstream.clone();
            for _ in 0..3 {
                let response = replace_response(
                    upstream_catalog(&payload, framed),
                    local.clone(),
                    &[1, 2, 15, 19, 20, 21],
                )
                .unwrap();
                let body = axum::body::to_bytes(response.into_body(), usize::MAX)
                    .await
                    .unwrap();
                let (is_framed, bytes) = unary_payload(&body).unwrap();
                assert_eq!(is_framed, framed);
                let catalog = AvailableModelsAddition::decode(bytes).unwrap();
                assert_eq!(catalog.model_names, ["local-model-hash"]);
                assert_eq!(catalog.models.len(), 1);
                assert_eq!(catalog.models[0].name, "local-model-hash");
                assert!(!catalog.models[0].variants.is_empty());
                let actual = CatalogMetadata::decode(bytes).unwrap();
                assert_eq!(actual.use_model_parameters, metadata.use_model_parameters);
                assert_eq!(actual.unrelated_metadata, metadata.unrelated_metadata);
                assert!(actual.experimental_model_id.is_none());
                assert!(actual.nudge_new_chats_to_auto_optimize_for.is_none());
                let picker =
                    PickerConfigurationFixture::decode(actual.display_configuration.as_slice())
                        .unwrap();
                assert_eq!(
                    picker
                        .routed_model_view_config
                        .unwrap()
                        .hide_routed_model_view,
                    Some(true)
                );
                let named = NamedPickerConfigurationFixture::decode(
                    picker.named_models_view_config.as_slice(),
                )
                .unwrap();
                assert!(named.auto_toggle.is_none());
                assert!(named.no_button.is_some());
                payload = bytes.to_vec();
            }
        }
    }

    #[tokio::test]
    async fn usable_catalog_replaces_official_models_even_when_local_catalog_is_empty() {
        let official = UsableModelsAddition {
            models: vec![agent::ModelDetails {
                model_id: "official-model".into(),
                ..Default::default()
            }],
        }
        .encode_to_vec();
        for models in [vec![usable_model(&model())], vec![]] {
            let local = UsableModelsAddition {
                models: models.clone(),
            }
            .encode_to_vec();
            let response =
                replace_response(upstream_catalog(&official, false), local, &[1]).unwrap();
            let body = axum::body::to_bytes(response.into_body(), usize::MAX)
                .await
                .unwrap();
            assert_eq!(UsableModelsAddition::decode(body).unwrap().models, models);
        }
    }

    #[test]
    fn cli_model_details_use_local_routing_credentials() {
        let details = usable_model(&model());
        assert_eq!(details.model_id, "local-model-hash");
        assert_eq!(details.display_name, "Local Model");
        let agent::model_details::Credentials::ApiKeyCredentials(credentials) =
            details.credentials.expect("API credentials")
        else {
            panic!("expected API key credentials");
        };
        assert_eq!(credentials.api_key, CLI_LOCAL_MODEL_API_KEY);
        assert_eq!(credentials.base_url, None);
        assert_ne!(credentials.api_key, "provider-secret");
    }

    #[test]
    fn cli_default_responses_use_the_local_model_hash() {
        let models = vec![model()];
        let details = default_model_details(&models).expect("default model");
        assert_eq!(details.model_id, "local-model-hash");

        let response = default_model_response(&models);
        assert_eq!(response.model, "local-model-hash");
        assert_eq!(response.thinking_model, "local-model-hash");

        let nudge = default_model_nudge_response(&models);
        assert_eq!(
            nudge.models_with_no_default_switch,
            vec!["local-model-hash"]
        );
    }
}
