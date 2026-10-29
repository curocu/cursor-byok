use std::collections::BTreeMap;

use crate::{
    cursor::protocol::proto::inference as pb,
    model::ProviderReplayState,
    provider::{FinishReason, ModelEvent},
    Error, Result,
};

#[derive(Default)]
pub struct InvocationOutput {
    text: String,
    thinking: String,
    tools: BTreeMap<usize, pb::InferenceToolCall>,
    replay: Option<ProviderReplayState>,
    pub done: bool,
}

impl InvocationOutput {
    pub fn event(
        &mut self,
        event: ModelEvent,
        model: &str,
    ) -> Result<Vec<pb::InferenceStreamResponse>> {
        use pb::inference_stream_response::Response as R;
        let response = match event {
            ModelEvent::TextDelta(text) => {
                self.text.push_str(&text);
                Some(R::TextPart(pb::InferenceTextStreamPart {
                    text,
                    is_final: false,
                }))
            }
            ModelEvent::TextEnd => None,
            ModelEvent::ThinkingDelta(text) => {
                self.thinking.push_str(&text);
                Some(R::ThinkingPart(pb::InferenceThinkingStreamPart {
                    text,
                    ..Default::default()
                }))
            }
            ModelEvent::ThinkingEnd => Some(R::ThinkingPart(pb::InferenceThinkingStreamPart {
                is_final: true,
                ..Default::default()
            })),
            ModelEvent::ToolCallStart {
                index,
                call_id,
                name,
            } => {
                self.tools.insert(
                    index,
                    pb::InferenceToolCall {
                        tool_call_id: call_id.clone(),
                        tool_name: name.clone(),
                        raw_tool_call_args: Some(String::new()),
                        ..Default::default()
                    },
                );
                Some(R::ToolCallPart(pb::InferenceToolCallStreamPart {
                    tool_call_id: call_id,
                    tool_name: name,
                    tool_index: Some(index as i32),
                    ..Default::default()
                }))
            }
            ModelEvent::ToolCallArgumentsDelta { index, delta } => {
                let tool = self.tools.get_mut(&index).ok_or_else(|| {
                    Error::Protocol("tool arguments received before tool start".into())
                })?;
                tool.raw_tool_call_args
                    .get_or_insert_default()
                    .push_str(&delta);
                Some(R::ToolCallPart(pb::InferenceToolCallStreamPart {
                    tool_call_id: tool.tool_call_id.clone(),
                    tool_name: String::new(),
                    args: delta,
                    tool_index: Some(index as i32),
                    ..Default::default()
                }))
            }
            ModelEvent::ToolCallEnd { index } => {
                let tool = self
                    .tools
                    .get_mut(&index)
                    .ok_or_else(|| Error::Protocol("tool end received before tool start".into()))?;
                let arguments: serde_json::Value =
                    serde_json::from_str(tool.raw_tool_call_args.as_deref().unwrap_or("{}"))?;
                tool.args = Some(super::input::proto_struct(&arguments)?);
                Some(R::ToolCallPart(pb::InferenceToolCallStreamPart {
                    tool_call_id: tool.tool_call_id.clone(),
                    tool_name: tool.tool_name.clone(),
                    args: tool.raw_tool_call_args.clone().unwrap_or_default(),
                    is_complete: true,
                    tool_index: Some(index as i32),
                    ..Default::default()
                }))
            }
            ModelEvent::Usage(usage) => {
                fn count(v: Option<u64>) -> i32 {
                    v.unwrap_or_default().min(i32::MAX as u64) as i32
                }
                return Ok(vec![
                    wrap(R::Usage(pb::InferenceUsageInfo {
                        prompt_tokens: count(usage.input_tokens),
                        completion_tokens: count(usage.output_tokens),
                        total_tokens: usage.total_tokens.map(|v| v.min(i32::MAX as u64) as i32),
                    })),
                    wrap(R::ExtendedUsage(pb::InferenceExtendedUsageInfo {
                        input_tokens: count(usage.input_tokens),
                        output_tokens: count(usage.output_tokens),
                        cache_read_tokens: count(usage.cache_read_tokens),
                        cache_write_tokens: count(usage.cache_write_tokens),
                        reasoning_tokens: usage
                            .reasoning_tokens
                            .map(|v| v.min(i32::MAX as u64) as i32),
                        ..Default::default()
                    })),
                ]);
            }
            ModelEvent::ProviderReplayState(state) => {
                self.replay = Some(state);
                None
            }
            ModelEvent::Done(reason) => {
                self.done = true;
                let native = self
                    .replay
                    .as_ref()
                    .filter(|s| s.provider_kind == "anthropic")
                    .and_then(|s| s.value.get("blocks"))
                    .map(serde_json::Value::to_string);
                let info = R::ResponseInfo(pb::InferenceResponseInfo {
                    id: uuid::Uuid::new_v4().to_string(),
                    model: model.into(),
                    created_at: chrono::Utc::now().timestamp_millis(),
                    messages: vec![pb::InferenceResponseMessage {
                        id: uuid::Uuid::new_v4().to_string(),
                        role: 2,
                        content: Some(self.text.clone()),
                        tool_calls: self.tools.values().cloned().collect(),
                        reasoning_parts: if self.thinking.is_empty() && self.replay.is_none() {
                            Vec::new()
                        } else {
                            vec![pb::InferenceReasoningPart {
                                text: self.thinking.clone(),
                                signature: self
                                    .replay
                                    .as_ref()
                                    .filter(|s| s.provider_kind != "anthropic")
                                    .map(|state| {
                                        serde_json::to_string(state)
                                            .map(|s| format!("{}{s}", super::input::REPLAY_PREFIX))
                                    })
                                    .transpose()?,
                                model_name: Some(model.into()),
                                ..Default::default()
                            }]
                        },
                        anthropic_native_content: native,
                        ..Default::default()
                    }],
                    // Output limits are reported through the stream error contract.
                    error_message: matches!(reason, FinishReason::Length)
                        .then(|| "maximum output tokens reached".into()),
                    ..Default::default()
                });
                return Ok(vec![
                    wrap(info),
                    wrap(R::TextPart(pb::InferenceTextStreamPart {
                        text: String::new(),
                        is_final: true,
                    })),
                ]);
            }
            ModelEvent::Start { .. } | ModelEvent::TextStart | ModelEvent::ThinkingStart => None,
        };
        Ok(response.into_iter().map(wrap).collect())
    }
}

fn wrap(response: pb::inference_stream_response::Response) -> pb::InferenceStreamResponse {
    pb::InferenceStreamResponse {
        response: Some(response),
        ..Default::default()
    }
}
