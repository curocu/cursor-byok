use base64::{engine::general_purpose::STANDARD, Engine};
use serde_json::{json, Value};

use crate::{
    cursor::protocol::proto::{agent::v1 as agent, inference as pb},
    model::{
        ContentPart, ModelInvocation, ModelRequest, ModelSpec, ProjectedContent, ProjectedMessage,
        PromptSpec, ProviderReplayState, Role, ToolCallContent, ToolDefinition, ToolResultContent,
    },
    Error, Result,
};

pub(super) const REPLAY_PREFIX: &str = "cursor-byok:replay:";

pub fn json_value(value: &prost_types::Value) -> Value {
    use prost_types::value::Kind;
    match value.kind.as_ref() {
        Some(Kind::NullValue(_)) | None => Value::Null,
        // Protobuf Struct stores every number as f64. Restore JSON integers so
        // strict provider schema validators accept minLength, minItems, etc.
        Some(Kind::NumberValue(v)) if v.fract() == 0.0 && v.abs() <= 9_007_199_254_740_991.0 => {
            json!(*v as i64)
        }
        Some(Kind::NumberValue(v)) => json!(v),
        Some(Kind::StringValue(v)) => json!(v),
        Some(Kind::BoolValue(v)) => json!(v),
        Some(Kind::StructValue(v)) => json_struct(v),
        Some(Kind::ListValue(v)) => v.values.iter().map(json_value).collect(),
    }
}

pub fn json_struct(value: &prost_types::Struct) -> Value {
    Value::Object(
        value
            .fields
            .iter()
            .map(|(k, v)| (k.clone(), json_value(v)))
            .collect(),
    )
}

pub fn proto_struct(value: &Value) -> Result<prost_types::Struct> {
    fn convert(value: &Value) -> prost_types::Value {
        use prost_types::value::Kind;
        let kind = match value {
            Value::Null => Kind::NullValue(0),
            Value::Bool(v) => Kind::BoolValue(*v),
            Value::Number(v) => Kind::NumberValue(v.as_f64().unwrap_or_default()),
            Value::String(v) => Kind::StringValue(v.clone()),
            Value::Array(v) => Kind::ListValue(prost_types::ListValue {
                values: v.iter().map(convert).collect(),
            }),
            Value::Object(v) => Kind::StructValue(prost_types::Struct {
                fields: v.iter().map(|(k, v)| (k.clone(), convert(v))).collect(),
            }),
        };
        prost_types::Value { kind: Some(kind) }
    }
    match value {
        Value::Object(v) => Ok(prost_types::Struct {
            fields: v.iter().map(|(k, v)| (k.clone(), convert(v))).collect(),
        }),
        _ => Err(Error::Protocol(
            "inference tool arguments must be a JSON object".into(),
        )),
    }
}

fn parts(parts: &[pb::InferenceContentPart]) -> Result<Vec<ContentPart>> {
    use pb::inference_content_part::Part;
    parts
        .iter()
        .map(|part| match part.part.as_ref() {
            Some(Part::Text(text)) => Ok(ContentPart::Text {
                text: text.text.clone(),
            }),
            Some(Part::Image(image)) => {
                let (mime_type, data) = if let Some(data) = image.data.strip_prefix("data:") {
                    let (header, data) = data
                        .split_once(",")
                        .ok_or_else(|| Error::Protocol("invalid image data URL".into()))?;
                    (header.trim_end_matches(";base64").to_owned(), data)
                } else {
                    (
                        image
                            .mime_type
                            .clone()
                            .unwrap_or_else(|| "image/png".into()),
                        image.data.as_str(),
                    )
                };
                Ok(ContentPart::Image {
                    mime_type,
                    data: STANDARD
                        .decode(data)
                        .map_err(|e| Error::Protocol(format!("invalid inference image: {e}")))?,
                })
            }
            Some(Part::File(_)) => Err(Error::Protocol(
                "inference file parts are not supported by the configured provider adapter".into(),
            )),
            None => Err(Error::Protocol("empty inference content part".into())),
        })
        .collect()
}

pub fn invocation(
    request: &pb::InferenceStreamRequest,
    model_id: &str,
    conversation_id: &str,
    run_id: &str,
    invocation_id: &str,
    index: u64,
) -> Result<ModelInvocation> {
    use pb::inference_core_message::Content;
    let mut instructions = Vec::new();
    let mut history = Vec::new();
    for (i, message) in request.messages.iter().enumerate() {
        let text = match message.content.as_ref() {
            Some(Content::Text(text)) => text.clone(),
            Some(Content::Parts(content)) => content
                .parts
                .iter()
                .filter_map(|p| match p.part.as_ref() {
                    Some(pb::inference_content_part::Part::Text(t)) => Some(t.text.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("\n"),
            _ => String::new(),
        };
        if message.role == 4 {
            instructions.push(text);
            continue;
        }
        if let Some(Content::ToolContent(content)) = message.content.as_ref() {
            for (j, result) in content.parts.iter().enumerate() {
                let value = result
                    .result
                    .as_ref()
                    .map(json_value)
                    .unwrap_or(Value::Null);
                history.push(ProjectedMessage {
                    message_id: format!("{i}:{j}"),
                    role: Role::Tool,
                    content: ProjectedContent::ToolResult(ToolResultContent {
                        call_id: result.tool_call_id.clone(),
                        name: result.tool_name.clone(),
                        content: value
                            .as_str()
                            .map(str::to_owned)
                            .unwrap_or_else(|| value.to_string()),
                        is_error: result.is_error
                            || result.cursor_tool_call_is_error.unwrap_or(false),
                        image: None,
                        provider_parts: parts(&result.experimental_content)?,
                    }),
                });
            }
            continue;
        }
        let content = if message.role == 2 {
            let replay_state = message
                .anthropic_native_content
                .as_deref()
                .map(serde_json::from_str::<Value>)
                .transpose()?
                .map(|content| ProviderReplayState {
                    provider_kind: "anthropic".into(),
                    value: json!({"blocks": content}),
                })
                .or(message.reasoning_parts.iter().find_map(|part| {
                    if part.model_name.as_deref() != Some(model_id) {
                        return None;
                    }
                    part.signature
                        .as_deref()
                        .and_then(|s| s.strip_prefix(REPLAY_PREFIX))
                        .and_then(|s| serde_json::from_str(s).ok())
                }));
            ProjectedContent::Assistant {
                text,
                thinking: message
                    .reasoning_parts
                    .iter()
                    .map(|p| p.text.as_str())
                    .collect::<Vec<_>>()
                    .join(""),
                replay_state,
                calls: message
                    .tool_calls
                    .iter()
                    .enumerate()
                    .map(|(index, call)| {
                        let arguments = if let Some(raw) = &call.raw_tool_call_args {
                            serde_json::from_str(raw)?
                        } else {
                            call.args
                                .as_ref()
                                .map(json_struct)
                                .unwrap_or_else(|| json!({}))
                        };
                        Ok(ToolCallContent {
                            index,
                            call_id: call.tool_call_id.clone(),
                            name: call.tool_name.clone(),
                            arguments,
                        })
                    })
                    .collect::<Result<_>>()?,
            }
        } else {
            ProjectedContent::Parts(match message.content.as_ref() {
                Some(Content::Parts(content)) => parts(&content.parts)?,
                _ => vec![ContentPart::Text { text }],
            })
        };
        history.push(ProjectedMessage {
            message_id: i.to_string(),
            role: if message.role == 2 {
                Role::Assistant
            } else {
                Role::User
            },
            content,
        });
    }
    if !request.provider_defined_tools.is_empty() {
        return Err(Error::Protocol(
            "provider-defined inference tools require a provider-specific adapter".into(),
        ));
    }
    let mut model = if let Some(selected) = &request.requested_model {
        crate::cursor::compile::model::from_requested(
            &agent::RequestedModel {
                model_id: model_id.into(),
                max_mode: selected.max_mode,
                parameters: selected
                    .parameters
                    .iter()
                    .map(|p| agent::requested_model::ModelParameterValue {
                        id: p.id.clone(),
                        value: p.value.clone(),
                    })
                    .collect(),
                ..Default::default()
            },
            None,
        )?
    } else {
        ModelSpec::new(model_id)
    };
    model.max_output_tokens = request
        .model_config
        .as_ref()
        .and_then(|c| c.max_tokens)
        .filter(|v| *v > 0)
        .map(|v| v as u64);
    if let Some(config) = &request.model_config {
        if let Some(v) = config.temperature {
            model.extra_params["temperature"] = json!(v);
        }
        if let Some(v) = config.top_p {
            model.extra_params["top_p"] = json!(v);
        }
    }
    Ok(ModelInvocation {
        call_id: format!("{run_id}:{invocation_id}"),
        run_id: run_id.into(),
        conversation_id: conversation_id.into(),
        provider_call_index: index,
        request: ModelRequest {
            prompt: PromptSpec {
                instructions: instructions.join("\n\n"),
                tools: request
                    .tools
                    .iter()
                    .map(|tool| ToolDefinition {
                        name: tool.name.clone(),
                        description: tool.description.clone(),
                        parameters: tool
                            .parameters
                            .as_ref()
                            .map(tool_schema)
                            .unwrap_or_else(|| json!({"type":"object","properties":{}})),
                    })
                    .collect(),
            },
            model,
            history,
        },
    })
}

// Cursor serializes the AI SDK schema object inside the protobuf Struct.
fn tool_schema(parameters: &prost_types::Struct) -> Value {
    let mut value = json_struct(parameters);
    value
        .as_object_mut()
        .and_then(|v| v.remove("jsonSchema"))
        .unwrap_or(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unwraps_cursor_ai_sdk_tool_schema_before_sending_to_provider() {
        let schema =
            json!({"type":"object","properties":{"path":{"type":"string"}},"required":["path"]});
        let parameters = proto_struct(&json!({"jsonSchema": schema})).unwrap();
        assert_eq!(tool_schema(&parameters), schema);
    }

    #[test]
    fn protobuf_numbers_restore_integers_at_every_nesting_level() {
        let schema = json!({"jsonSchema": {"type":"object", "properties": {
            "query": {"type":"string", "minLength":1, "maxLength":256},
            "items": {"type":"array", "minItems":1, "maxItems":10},
            "number": {"type":"number", "minimum":-2, "multipleOf":0.25}
        }, "allOf":[{"minProperties":1}]}});
        let parameters = proto_struct(&schema).unwrap();
        let decoded = prost::Message::encode_to_vec(&parameters);
        let decoded: prost_types::Struct = prost::Message::decode(decoded.as_slice()).unwrap();
        let actual = tool_schema(&decoded);
        assert_eq!(actual["properties"]["query"]["minLength"].as_i64(), Some(1));
        assert_eq!(actual["properties"]["items"]["maxItems"].as_i64(), Some(10));
        assert_eq!(actual["allOf"][0]["minProperties"].as_i64(), Some(1));
        assert_eq!(
            actual["properties"]["number"]["multipleOf"].as_f64(),
            Some(0.25)
        );
    }
}
