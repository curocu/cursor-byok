use std::collections::BTreeMap;

use crate::{
    cursor::protocol::proto::inference as pb,
    model::{AssistantPart, ProviderReplayState},
    provider::{FinishReason, ModelEvent},
    Error, Result,
};

const OUTPUT_TOKEN_LIMIT_MESSAGE: &str = "Provider exceeded max output tokens";

#[derive(Default)]
pub struct InvocationOutput {
    max_context_tokens: i32,
    text: String,
    parts: Vec<AssistantPart>,
    text_part: Option<usize>,
    thinking_part: Option<usize>,
    tools: BTreeMap<usize, pb::InferenceToolCall>,
    replay: Option<ProviderReplayState>,
    pub done: bool,
}

impl InvocationOutput {
    pub fn new(max_context_tokens: i32) -> Self {
        Self {
            max_context_tokens,
            ..Default::default()
        }
    }

    pub fn event(
        &mut self,
        event: ModelEvent,
        model: &str,
    ) -> Result<Vec<pb::InferenceStreamResponse>> {
        use pb::inference_stream_response::Response as R;
        let response = match event {
            ModelEvent::TextStart => {
                self.text_part = None;
                None
            }
            ModelEvent::TextDelta(text) => {
                self.text.push_str(&text);
                if !text.is_empty() {
                    let index = *self.text_part.get_or_insert_with(|| {
                        self.parts.push(AssistantPart::Text(String::new()));
                        self.parts.len() - 1
                    });
                    if let AssistantPart::Text(part) = &mut self.parts[index] {
                        part.push_str(&text);
                    }
                }
                Some(R::TextPart(pb::InferenceTextStreamPart {
                    text,
                    is_final: false,
                }))
            }
            ModelEvent::TextEnd => {
                self.text_part = None;
                None
            }
            ModelEvent::ThinkingStart => {
                self.thinking_part = None;
                self.text_part = None;
                None
            }
            ModelEvent::ThinkingDelta(text) => {
                let index = *self.thinking_part.get_or_insert_with(|| {
                    self.parts.push(AssistantPart::Reasoning {
                        text: String::new(),
                        replay_state: None,
                    });
                    self.parts.len() - 1
                });
                if let AssistantPart::Reasoning { text: part, .. } = &mut self.parts[index] {
                    part.push_str(&text);
                }
                Some(R::ThinkingPart(pb::InferenceThinkingStreamPart {
                    text,
                    ..Default::default()
                }))
            }
            ModelEvent::ThinkingEnd => {
                self.thinking_part = None;
                Some(R::ThinkingPart(pb::InferenceThinkingStreamPart {
                    is_final: true,
                    ..Default::default()
                }))
            }
            ModelEvent::ReasoningReplayState(state) => {
                if let Some(AssistantPart::Reasoning { replay_state, .. }) =
                    self.parts.last_mut().filter(|p| {
                        matches!(
                            p,
                            AssistantPart::Reasoning {
                                replay_state: None,
                                ..
                            }
                        )
                    })
                {
                    *replay_state = Some(state);
                } else {
                    self.parts.push(AssistantPart::Reasoning {
                        text: String::new(),
                        replay_state: Some(state),
                    });
                }
                self.text_part = None;
                None
            }
            ModelEvent::ToolCallStart {
                index,
                call_id,
                name,
            } => {
                self.text_part = None;
                self.thinking_part = None;
                self.parts.push(AssistantPart::ToolCall(index));
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
                let raw = tool.raw_tool_call_args.as_deref().unwrap_or("{}");
                let arguments: serde_json::Value = match serde_json::from_str(raw) {
                    Ok(value) => value,
                    // A provider can send args.done before reporting a length limit.
                    // Wait for Done before deciding whether this is a truncated call.
                    Err(error) if error.is_eof() => return Ok(Vec::new()),
                    Err(error) => return Err(error.into()),
                };
                tool.args = Some(super::input::proto_struct(&arguments)?);
                Some(R::ToolCallPart(pb::InferenceToolCallStreamPart {
                    tool_call_id: tool.tool_call_id.clone(),
                    tool_name: tool.tool_name.clone(),
                    args: raw.into(),
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
                        max_tokens: self.max_context_tokens,
                        reasoning_tokens: usage
                            .reasoning_tokens
                            .map(|v| v.min(i32::MAX as u64) as i32),
                        context_input_tokens: usage
                            .context_input_tokens
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
                if reason != FinishReason::Length {
                    if let Some(tool) = self.tools.values().find(|t| t.args.is_none()) {
                        serde_json::from_str::<serde_json::Value>(
                            tool.raw_tool_call_args.as_deref().unwrap_or(""),
                        )?;
                        return Err(Error::Protocol(
                            "provider completed without ending a tool call".into(),
                        ));
                    }
                }
                let native = self
                    .replay
                    .as_ref()
                    .filter(|s| s.provider_kind == "anthropic")
                    .and_then(|s| s.value.get("blocks"))
                    .map(serde_json::Value::to_string);
                let mut parts = self.parts.clone();
                if let Some(state) = self
                    .replay
                    .as_ref()
                    .filter(|s| s.provider_kind != "anthropic")
                {
                    if !parts.iter().any(|p| {
                        matches!(
                            p,
                            AssistantPart::Reasoning {
                                replay_state: Some(_),
                                ..
                            }
                        )
                    }) {
                        if let Some(AssistantPart::Reasoning { replay_state, .. }) = parts
                            .iter_mut()
                            .find(|p| matches!(p, AssistantPart::Reasoning { .. }))
                        {
                            *replay_state = Some(state.clone());
                        } else {
                            parts.insert(
                                0,
                                AssistantPart::Reasoning {
                                    text: String::new(),
                                    replay_state: Some(state.clone()),
                                },
                            );
                        }
                    }
                }
                let mut message = pb::InferenceResponseMessage {
                    id: uuid::Uuid::new_v4().to_string(),
                    role: 2,
                    content: Some(self.text.clone()),
                    anthropic_native_content: native,
                    ..Default::default()
                };
                for part in parts {
                    match part {
                        AssistantPart::Text(text) if !text.is_empty() => {
                            message.assistant_part_order.push(2);
                            message.assistant_text_parts.push(text);
                        }
                        AssistantPart::Reasoning { text, replay_state } => {
                            message.assistant_part_order.push(1);
                            message.reasoning_parts.push(pb::InferenceReasoningPart {
                                text,
                                signature: replay_state
                                    .as_ref()
                                    .map(|state| {
                                        serde_json::to_string(state)
                                            .map(|s| format!("{}{s}", super::input::REPLAY_PREFIX))
                                    })
                                    .transpose()?,
                                model_name: Some(model.into()),
                                ..Default::default()
                            });
                        }
                        AssistantPart::ToolCall(index) => {
                            if let Some(tool) = self.tools.get(&index).filter(|t| t.args.is_some())
                            {
                                message.assistant_part_order.push(3);
                                message.tool_calls.push(tool.clone());
                            }
                        }
                        AssistantPart::Text(_) => {}
                    }
                }
                let info = R::ResponseInfo(pb::InferenceResponseInfo {
                    id: uuid::Uuid::new_v4().to_string(),
                    model: model.into(),
                    created_at: chrono::Utc::now().timestamp_millis(),
                    supports_self_summary: Some(true),
                    messages: vec![message],
                    error_message: (reason == FinishReason::Length)
                        .then(|| OUTPUT_TOKEN_LIMIT_MESSAGE.into()),
                    ..Default::default()
                });
                let mut responses = Vec::with_capacity(3);
                if reason == FinishReason::Length {
                    responses.push(wrap(R::Error(pb::InferenceStreamError {
                        message: OUTPUT_TOKEN_LIMIT_MESSAGE.into(),
                        code: "OUTPUT_TOKEN_LIMIT".into(),
                        is_input_token_limit_error: false,
                        is_output_token_limit_error: true,
                        error_type:
                            pb::InferenceStreamErrorType::InferencestreamerrortypeOutputTokenLimit
                                as i32,
                    })));
                }
                responses.extend([
                    wrap(info),
                    wrap(R::TextPart(pb::InferenceTextStreamPart {
                        text: String::new(),
                        is_final: true,
                    })),
                ]);
                self.done = true;
                return Ok(responses);
            }
            ModelEvent::Start { .. } => None,
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

#[cfg(test)]
mod tests {
    use super::*;
    use prost::Message;

    // Decode the contract independently of BYOK's generated schema.
    #[derive(Clone, PartialEq, Message)]
    struct CurrentContextUsage {
        #[prost(int32, optional, tag = "7")]
        context_input_tokens: Option<i32>,
    }

    #[test]
    fn current_context_usage_reaches_cursor_without_accumulating_billing_tokens() {
        let mut output = InvocationOutput::new(128_000);
        for (context, expected) in [
            (Some(100_766), Some(100_766)),
            (Some(800), Some(800)),
            (Some(0), Some(0)),
            (None, None),
            (Some(u64::MAX), Some(i32::MAX)),
        ] {
            let responses = output
                .event(
                    ModelEvent::Usage(crate::model::Usage {
                        input_tokens: Some(414),
                        context_input_tokens: context,
                        output_tokens: Some(20),
                        total_tokens: Some(999_999),
                        cache_read_tokens: Some(100_352),
                        ..Default::default()
                    }),
                    "model",
                )
                .unwrap();
            let usage = responses
                .into_iter()
                .find_map(|r| match r.response {
                    Some(pb::inference_stream_response::Response::ExtendedUsage(u)) => Some(u),
                    _ => None,
                })
                .unwrap();
            assert_eq!(usage.input_tokens, 414);
            assert_eq!(usage.cache_read_tokens, 100_352);
            assert_eq!(usage.max_tokens, 128_000);
            let wire = CurrentContextUsage::decode(usage.encode_to_vec().as_slice()).unwrap();
            assert_eq!(wire.context_input_tokens, expected);
        }
    }

    fn final_message(output: &mut InvocationOutput) -> pb::InferenceResponseMessage {
        output
            .event(ModelEvent::Done(FinishReason::Stop), "model")
            .unwrap()
            .into_iter()
            .find_map(|r| match r.response {
                Some(pb::inference_stream_response::Response::ResponseInfo(mut info)) => {
                    Some(info.messages.remove(0))
                }
                _ => None,
            })
            .unwrap()
    }

    #[test]
    fn truncated_tool_on_normal_completion_remains_an_error() {
        let mut output = InvocationOutput::default();
        for event in [
            ModelEvent::ToolCallStart {
                index: 0,
                call_id: "tool".into(),
                name: "Read".into(),
            },
            ModelEvent::ToolCallArgumentsDelta {
                index: 0,
                delta: "{\"path\":\"unfinished".into(),
            },
            ModelEvent::ToolCallEnd { index: 0 },
        ] {
            assert!(!output
                .event(event, "model")
                .unwrap()
                .iter()
                .any(|r| matches!(&r.response,
                Some(pb::inference_stream_response::Response::ToolCallPart(p)) if p.is_complete)));
        }
        assert!(output
            .event(ModelEvent::Done(FinishReason::ToolUse), "model")
            .is_err());
        assert!(!output.done);
    }

    #[test]
    fn mixed_output_keeps_first_seen_tool_order_and_reasoning_blocks() {
        let mut output = InvocationOutput::default();
        for event in [
            ModelEvent::TextDelta("before".into()),
            ModelEvent::ThinkingStart,
            ModelEvent::ThinkingDelta("why".into()),
            ModelEvent::ThinkingEnd,
            ModelEvent::ToolCallStart {
                index: 9,
                call_id: "first".into(),
                name: "Read".into(),
            },
            ModelEvent::ToolCallArgumentsDelta {
                index: 9,
                delta: "{}".into(),
            },
            ModelEvent::ToolCallEnd { index: 9 },
            ModelEvent::TextDelta("middle".into()),
            ModelEvent::ToolCallStart {
                index: 2,
                call_id: "second".into(),
                name: "Read".into(),
            },
            ModelEvent::ToolCallArgumentsDelta {
                index: 2,
                delta: "{}".into(),
            },
            ModelEvent::ToolCallEnd { index: 2 },
            ModelEvent::TextDelta("after".into()),
        ] {
            output.event(event, "model").unwrap();
        }
        let message = final_message(&mut output);
        assert_eq!(message.assistant_part_order, [2, 1, 3, 2, 3, 2]);
        assert_eq!(message.assistant_text_parts, ["before", "middle", "after"]);
        assert_eq!(message.reasoning_parts[0].text, "why");
        assert_eq!(
            message
                .tool_calls
                .iter()
                .map(|c| c.tool_call_id.as_str())
                .collect::<Vec<_>>(),
            ["first", "second"]
        );
    }

    #[test]
    fn plain_text_stream_and_final_snapshot_agree() {
        let mut output = InvocationOutput::default();
        let mut streamed = String::new();
        for text in ["普通", "输出", "🙂"] {
            for response in output
                .event(ModelEvent::TextDelta(text.into()), "model")
                .unwrap()
            {
                if let Some(pb::inference_stream_response::Response::TextPart(part)) =
                    response.response
                {
                    assert!(!part.is_final);
                    streamed.push_str(&part.text);
                }
            }
        }
        let message = final_message(&mut output);
        assert_eq!(message.content.as_deref(), Some(streamed.as_str()));
        assert_eq!(message.assistant_text_parts, [streamed]);
        assert_eq!(message.assistant_part_order, [2]);
    }

    #[test]
    fn output_limit_preserves_partial_output_and_reports_native_error() {
        use pb::inference_stream_response::Response as R;
        let mut output = InvocationOutput::new(128_000);
        output
            .event(ModelEvent::TextDelta("partial answer".into()), "model")
            .unwrap();
        let responses = output
            .event(ModelEvent::Done(FinishReason::Length), "model")
            .unwrap();
        let responses: Vec<_> = responses
            .iter()
            .map(|response| {
                pb::InferenceStreamResponse::decode(response.encode_to_vec().as_slice()).unwrap()
            })
            .collect();
        assert!(output.done);
        assert!(responses.iter().any(|response| matches!(&response.response,
            Some(R::ResponseInfo(info)) if info.messages[0].content.as_deref() == Some("partial answer")
                && info.error_message.as_deref() == Some(OUTPUT_TOKEN_LIMIT_MESSAGE))));
        assert_eq!(responses.len(), 3);
        assert!(matches!(&responses[0].response, Some(R::Error(_))));
        assert!(matches!(&responses[1].response, Some(R::ResponseInfo(_))));
        assert!(matches!(&responses[2].response,
            Some(R::TextPart(part)) if part.is_final && part.text.is_empty()));
        let errors: Vec<_> = responses
            .iter()
            .filter_map(|response| match &response.response {
                Some(R::Error(error)) => Some(error),
                _ => None,
            })
            .collect();
        assert_eq!(errors.len(), 1);
        assert!(errors[0].is_output_token_limit_error);
        assert!(!errors[0].is_input_token_limit_error);
        assert_eq!(
            errors[0].error_type,
            pb::InferenceStreamErrorType::InferencestreamerrortypeOutputTokenLimit as i32
        );
    }

    #[test]
    fn normal_completion_does_not_report_output_limit() {
        use pb::inference_stream_response::Response as R;
        for reason in [FinishReason::Stop, FinishReason::ToolUse] {
            let mut output = InvocationOutput::default();
            let responses = output.event(ModelEvent::Done(reason), "model").unwrap();
            assert!(output.done);
            assert!(!responses
                .iter()
                .any(|response| matches!(&response.response, Some(R::Error(_)))));
            assert!(responses.iter().any(|response| matches!(&response.response,
                Some(R::ResponseInfo(info)) if info.error_message.is_none())));
        }
    }
}
