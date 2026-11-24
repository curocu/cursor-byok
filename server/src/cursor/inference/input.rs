use std::collections::HashSet;

use base64::{engine::general_purpose::STANDARD, Engine};
use serde_json::{json, Value};

use crate::{
    cursor::protocol::proto::{agent::v1 as agent, inference as pb},
    model::{
        AssistantPart, ContentPart, ModelInvocation, ModelRequest, ModelSpec, ProjectedContent,
        ProjectedMessage, PromptSpec, ProviderReplayState, Role, ToolCallContent, ToolDefinition,
        ToolResultContent,
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
    // A stopped argument stream may have no result yet. Never discard a call
    // that Cursor reports as successfully executed, even if its JSON is damaged.
    let successful_calls: HashSet<&str> = request
        .messages
        .iter()
        .filter_map(|message| match &message.content {
            Some(Content::ToolContent(content)) => Some(&content.parts),
            _ => None,
        })
        .flatten()
        .filter(|result| !result.is_error && !result.cursor_tool_call_is_error.unwrap_or(false))
        .map(|result| result.tool_call_id.as_str())
        .collect();
    let mut interrupted_calls = HashSet::new();
    for (i, message) in request.messages.iter().enumerate() {
        let mut text = match message.content.as_ref() {
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
                if interrupted_calls.contains(result.tool_call_id.as_str()) {
                    let mut content = vec![ContentPart::Text {
                        text: format!(
                            "Historical interrupted tool call result ({}): {}",
                            result.tool_name,
                            value
                                .as_str()
                                .map(str::to_owned)
                                .unwrap_or_else(|| value.to_string())
                        ),
                    }];
                    content.extend(parts(&result.experimental_content)?);
                    history.push(ProjectedMessage {
                        message_id: format!("{i}:{j}"),
                        role: Role::User,
                        content: ProjectedContent::Parts(content),
                    });
                    continue;
                }
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
        let original_text = text.clone();
        let content = if message.role == 2 {
            let mut calls = Vec::new();
            for (index, call) in message.tool_calls.iter().enumerate() {
                let arguments = if let Some(raw) = &call.raw_tool_call_args {
                    match serde_json::from_str(raw) {
                        Ok(arguments) => arguments,
                        Err(error)
                            if error.is_eof()
                                && !call.tool_call_id.is_empty()
                                && !successful_calls.contains(call.tool_call_id.as_str()) =>
                        {
                            interrupted_calls.insert(call.tool_call_id.as_str());
                            text.push_str(&format!(
                                "\n[Historical {} call was interrupted before its JSON arguments were complete. It was not executed; do not restart it without a new request.]",
                                call.tool_name
                            ));
                            tracing::warn!(
                                tool_call_id = %call.tool_call_id,
                                tool_name = %call.tool_name,
                                "omitting interrupted tool arguments from inference history"
                            );
                            continue;
                        }
                        Err(error) => return Err(error.into()),
                    }
                } else {
                    call.args
                        .as_ref()
                        .map(json_struct)
                        .unwrap_or_else(|| json!({}))
                };
                calls.push(ToolCallContent {
                    index,
                    call_id: call.tool_call_id.clone(),
                    name: call.tool_name.clone(),
                    arguments,
                });
            }
            let mut replay_state = message
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
            if let Some(blocks) = replay_state
                .as_mut()
                .filter(|state| state.provider_kind == "anthropic")
                .and_then(|state| state.value.get_mut("blocks"))
                .and_then(Value::as_array_mut)
            {
                blocks.retain(|block| {
                    !(block["type"] == "tool_use"
                        && block["id"]
                            .as_str()
                            .is_some_and(|id| interrupted_calls.contains(id)))
                });
            }
            let mut ordered = assistant_parts(message, &calls, &original_text, model_id);
            if !ordered.is_empty() && text.len() > original_text.len() {
                ordered.push(AssistantPart::Text(text[original_text.len()..].into()));
            }
            ProjectedContent::Assistant {
                parts: ordered,
                text,
                thinking: message
                    .reasoning_parts
                    .iter()
                    .map(|p| p.text.as_str())
                    .collect::<Vec<_>>()
                    .join(""),
                replay_state,
                calls,
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

// Match the harness validation: inconsistent ordering metadata uses the flat form.
fn assistant_parts(
    message: &pb::InferenceCoreMessage,
    calls: &[ToolCallContent],
    text: &str,
    model_id: &str,
) -> Vec<AssistantPart> {
    if message.assistant_part_order.is_empty() {
        return Vec::new();
    }
    let texts = if message.assistant_text_parts.is_empty() && !text.is_empty() {
        vec![text.to_owned()]
    } else {
        message.assistant_text_parts.clone()
    };
    let count = |kind| {
        message
            .assistant_part_order
            .iter()
            .filter(|&&v| v == kind)
            .count()
    };
    if count(1) != message.reasoning_parts.len()
        || count(2) != texts.len()
        || count(3) != message.tool_calls.len()
        || count(1) + count(2) + count(3) != message.assistant_part_order.len()
        || texts.concat() != text
    {
        return Vec::new();
    }
    let (mut reasoning, mut text_index, mut tool) = (0, 0, 0);
    let mut parts = Vec::new();
    for kind in &message.assistant_part_order {
        match kind {
            1 => {
                let part = &message.reasoning_parts[reasoning];
                reasoning += 1;
                parts.push(AssistantPart::Reasoning {
                    text: part.text.clone(),
                    replay_state: part
                        .signature
                        .as_deref()
                        .filter(|_| part.model_name.as_deref() == Some(model_id))
                        .and_then(|s| s.strip_prefix(REPLAY_PREFIX))
                        .and_then(|s| serde_json::from_str(s).ok()),
                });
            }
            2 => {
                parts.push(AssistantPart::Text(texts[text_index].clone()));
                text_index += 1;
            }
            _ => {
                let call = &message.tool_calls[tool];
                tool += 1;
                if let Some(index) = calls.iter().position(|c| c.call_id == call.tool_call_id) {
                    parts.push(AssistantPart::ToolCall(index));
                }
            }
        }
    }
    parts
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

    fn interrupted_task_request() -> pb::InferenceStreamRequest {
        use pb::inference_core_message::Content;
        pb::InferenceStreamRequest {
            messages: vec![
                pb::InferenceCoreMessage {
                    role: 2,
                    content: Some(Content::Text("I will check the existing files.".into())),
                    tool_calls: vec![
                        pb::InferenceToolCall {
                            tool_call_id: "read-completed".into(),
                            tool_name: "Read".into(),
                            raw_tool_call_args: Some(r#"{"path":"a.txt"}"#.into()),
                            ..Default::default()
                        },
                        pb::InferenceToolCall {
                            tool_call_id: "task-interrupted".into(),
                            tool_name: "Task".into(),
                            raw_tool_call_args: Some(
                                r#"{"description":"研究分炉规则","prompt":"半截参数"#.into(),
                            ),
                            ..Default::default()
                        },
                    ],
                    ..Default::default()
                },
                pb::InferenceCoreMessage {
                    role: 3,
                    content: Some(Content::ToolContent(pb::InferenceToolResultContent {
                        parts: vec![
                            pb::InferenceToolResultPart {
                                tool_call_id: "read-completed".into(),
                                tool_name: "Read".into(),
                                result: Some(prost_types::Value {
                                    kind: Some(prost_types::value::Kind::StringValue(
                                        "file contents".into(),
                                    )),
                                }),
                                ..Default::default()
                            },
                            pb::InferenceToolResultPart {
                                tool_call_id: "task-interrupted".into(),
                                tool_name: "Task".into(),
                                result: Some(prost_types::Value {
                                    kind: Some(prost_types::value::Kind::StringValue(
                                        "Tool call arguments were not valid JSON".into(),
                                    )),
                                }),
                                is_error: true,
                                ..Default::default()
                            },
                        ],
                    })),
                    ..Default::default()
                },
                pb::InferenceCoreMessage {
                    role: 1,
                    content: Some(Content::Text(
                        "已经停止，可以先执行两个。不要重新启动旧 Task。".into(),
                    )),
                    ..Default::default()
                },
            ],
            ..Default::default()
        }
    }

    #[test]
    fn invalid_order_metadata_uses_flat_history() {
        let mut request = interrupted_task_request();
        for (order, texts) in [
            (vec![2, 3], vec!["I will check the existing files.".into()]),
            (
                vec![2, 3, 3, 7],
                vec!["I will check the existing files.".into()],
            ),
            (vec![2, 3, 3], vec!["mismatched text".into()]),
        ] {
            request.messages[0].assistant_part_order = order;
            request.messages[0].assistant_text_parts = texts;
            let input = invocation(&request, "model", "conversation", "run", "resume", 0).unwrap();
            assert!(matches!(&input.request.history[0].content,
                ProjectedContent::Assistant { parts, calls, text, .. } if parts.is_empty() && calls.len() == 1 && text.contains("interrupted")));
        }
    }

    #[test]
    fn ordered_history_skips_interrupted_tool_and_retains_followup_note() {
        let mut request = interrupted_task_request();
        request.messages[0].assistant_part_order = vec![2, 3, 3];
        let input = invocation(&request, "model", "conversation", "run", "resume", 0).unwrap();
        let ProjectedContent::Assistant { parts, .. } = &input.request.history[0].content else {
            panic!()
        };
        assert_eq!(parts.len(), 3);
        assert!(
            matches!(&parts[0], AssistantPart::Text(t) if t == "I will check the existing files.")
        );
        assert!(matches!(&parts[1], AssistantPart::ToolCall(0)));
        assert!(matches!(&parts[2], AssistantPart::Text(t) if t.contains("interrupted")));
    }

    #[test]
    fn ordered_reasoning_replay_is_isolated_by_model() {
        let mut request = pb::InferenceStreamRequest::default();
        request.messages.push(pb::InferenceCoreMessage {
            role: 2,
            assistant_part_order: vec![1],
            reasoning_parts: vec![pb::InferenceReasoningPart {
                text: "why".into(),
                model_name: Some("model".into()),
                signature: Some(format!(
                    "{REPLAY_PREFIX}{}",
                    json!({"provider_kind":"openai_responses", "value":{"items":[]}})
                )),
                ..Default::default()
            }],
            ..Default::default()
        });
        for (model, expected) in [("model", true), ("other-model", false)] {
            let input = invocation(&request, model, "conversation", "run", "resume", 0).unwrap();
            let ProjectedContent::Assistant {
                parts,
                replay_state,
                ..
            } = &input.request.history[0].content
            else {
                panic!()
            };
            assert_eq!(replay_state.is_some(), expected);
            assert!(
                matches!(&parts[0], AssistantPart::Reasoning { text, replay_state } if text == "why" && replay_state.is_some() == expected)
            );
        }
    }

    #[test]
    fn interrupted_task_history_preserves_completed_tools_and_followup() {
        use pb::inference_core_message::Content;
        for (is_error, cursor_error) in [(true, None), (false, Some(true))] {
            let mut request = interrupted_task_request();
            let Some(Content::ToolContent(results)) = &mut request.messages[1].content else {
                unreachable!()
            };
            results.parts[1].is_error = is_error;
            results.parts[1].cursor_tool_call_is_error = cursor_error;
            let input = invocation(&request, "model", "conversation", "run", "resume", 0).unwrap();
            let history = &input.request.history;
            let ProjectedContent::Assistant { text, calls, .. } = &history[0].content else {
                panic!("expected assistant history");
            };
            assert!(text.starts_with("I will check the existing files."));
            assert!(text.contains("Task") && text.contains("interrupted"));
            assert!(!text.contains("半截参数"));
            assert_eq!(calls.len(), 1);
            assert_eq!(calls[0].call_id, "read-completed");
            assert_eq!(calls[0].arguments, json!({"path":"a.txt"}));
            assert!(
                matches!(&history[1].content, ProjectedContent::ToolResult(r)
                if r.call_id == "read-completed" && r.content == "file contents")
            );
            assert!(matches!(&history[2].content, ProjectedContent::Parts(parts)
                if matches!(&parts[0], ContentPart::Text { text } if text.contains("Tool call arguments were not valid JSON"))));
            assert_eq!(history[2].role, Role::User);
            assert!(matches!(&history[3].content, ProjectedContent::Parts(parts)
                if matches!(&parts[0], ContentPart::Text { text } if text.contains("可以先执行两个"))));
        }
    }

    #[test]
    fn interrupted_task_without_result_does_not_block_followup() {
        use pb::inference_core_message::Content;
        let mut request = interrupted_task_request();
        let Some(Content::ToolContent(results)) = &mut request.messages[1].content else {
            unreachable!()
        };
        results.parts.pop();
        assert!(invocation(&request, "model", "conversation", "run", "resume", 0).is_ok());
    }

    #[test]
    fn successful_or_non_truncated_invalid_calls_are_not_silently_removed() {
        use pb::inference_core_message::Content;
        let mut request = interrupted_task_request();
        let Some(Content::ToolContent(results)) = &mut request.messages[1].content else {
            unreachable!()
        };
        results.parts[1].is_error = false;
        assert!(invocation(&request, "model", "conversation", "run", "resume", 0).is_err());

        let mut request = interrupted_task_request();
        request.messages[0].tool_calls[1].raw_tool_call_args = Some("{invalid-json}".into());
        assert!(invocation(&request, "model", "conversation", "run", "resume", 0).is_err());

        let mut request = interrupted_task_request();
        request.messages[0].tool_calls[1].tool_call_id.clear();
        assert!(invocation(&request, "model", "conversation", "run", "resume", 0).is_err());
    }

    #[test]
    fn interrupted_task_preserves_signed_thinking_without_replaying_native_tool_use() {
        let mut request = interrupted_task_request();
        let thinking = json!({"type":"thinking", "thinking":"Reasoning", "signature":"signed"});
        request.messages[0].anthropic_native_content = Some(
            json!([
                thinking.clone(),
                {"type":"tool_use", "id":"task-interrupted", "name":"Task", "input":{}}
            ])
            .to_string(),
        );
        let input = invocation(&request, "model", "conversation", "run", "resume", 0).unwrap();
        let ProjectedContent::Assistant {
            replay_state: Some(state),
            ..
        } = &input.request.history[0].content
        else {
            panic!("expected native replay state");
        };
        assert_eq!(state.value["blocks"], json!([thinking]));
    }

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
