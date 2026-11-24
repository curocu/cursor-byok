//! Adapts Cursor's local Agent loop to the existing model providers.
mod input;
mod metadata;
mod output;

pub use input::{invocation, json_struct, json_value};
pub use metadata::prompt_metadata;
pub use output::InvocationOutput;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        cursor::protocol::proto::inference as pb,
        model::{ProjectedContent, ProviderReplayState},
        provider::{FinishReason, ModelEvent},
    };
    use prost::Message;
    use serde_json::json;

    #[test]
    fn plan_switches_and_todo_updates_preserve_tool_arguments_and_results() {
        let cases = [
            (
                "SwitchMode",
                json!({"target_mode_id":"plan","explanation":"先制定计划"}),
            ),
            (
                "CreatePlan",
                json!({"name":"模式验证","overview":"验证计划与待办","plan":"# 模式验证\n核对待办状态。","todos":[{"id":"verify","content":"核对待办"}]}),
            ),
            ("SwitchMode", json!({"target_mode_id":"agent"})),
            (
                "TodoWrite",
                json!({"merge":false,"todos":[{"id":"verify","content":"核对待办","status":"pending"},{"id":"summary","content":"汇总结果","status":"pending"}]}),
            ),
            (
                "TodoWrite",
                json!({"merge":true,"todos":[{"id":"verify","content":"核对待办","status":"in_progress"}]}),
            ),
            (
                "TodoWrite",
                json!({"merge":true,"todos":[{"id":"verify","content":"核对待办","status":"completed"},{"id":"summary","content":"汇总结果","status":"completed"}]}),
            ),
        ];
        for (name, arguments) in cases {
            let raw = arguments.to_string();
            let split = raw.char_indices().nth(raw.chars().count() / 2).unwrap().0;
            let mut output = InvocationOutput::default();
            for event in [
                ModelEvent::ToolCallStart {
                    index: 0,
                    call_id: "call-1".into(),
                    name: name.into(),
                },
                ModelEvent::ToolCallArgumentsDelta {
                    index: 0,
                    delta: raw[..split].into(),
                },
                ModelEvent::ToolCallArgumentsDelta {
                    index: 0,
                    delta: raw[split..].into(),
                },
                ModelEvent::ToolCallEnd { index: 0 },
            ] {
                output.event(event, "model").unwrap();
            }
            let responses = output
                .event(ModelEvent::Done(FinishReason::ToolUse), "model")
                .unwrap();
            let wire = pb::InferenceStreamResponse::decode(responses[0].encode_to_vec().as_slice())
                .unwrap();
            let Some(pb::inference_stream_response::Response::ResponseInfo(info)) = wire.response
            else {
                panic!("expected tool response info");
            };
            let assistant = info.messages.into_iter().next().unwrap();
            let call = &assistant.tool_calls[0];
            assert_eq!(call.tool_name, name);
            assert_eq!(call.raw_tool_call_args.as_deref(), Some(raw.as_str()));
            assert_eq!(json_struct(call.args.as_ref().unwrap()), arguments);
            let result = if name == "TodoWrite" {
                json!({"success":true,"finalTodos":arguments["todos"]})
            } else {
                json!({"success":true})
            };
            let request = pb::InferenceStreamRequest {
                messages: vec![
                    pb::InferenceCoreMessage {
                        role: 2,
                        tool_calls: assistant.tool_calls,
                        ..Default::default()
                    },
                    pb::InferenceCoreMessage {
                        role: 3,
                        content: Some(pb::inference_core_message::Content::ToolContent(
                            pb::InferenceToolResultContent {
                                parts: vec![pb::InferenceToolResultPart {
                                    tool_call_id: "call-1".into(),
                                    tool_name: name.into(),
                                    result: Some(prost_types::Value {
                                        kind: Some(prost_types::value::Kind::StructValue(
                                            input::proto_struct(&result).unwrap(),
                                        )),
                                    }),
                                    ..Default::default()
                                }],
                            },
                        )),
                        ..Default::default()
                    },
                ],
                ..Default::default()
            };
            let request =
                pb::InferenceStreamRequest::decode(request.encode_to_vec().as_slice()).unwrap();
            let projected =
                invocation(&request, "model", "conversation", "run", "invocation", 0).unwrap();
            let ProjectedContent::Assistant { calls, .. } = &projected.request.history[0].content
            else {
                panic!("expected assistant tool call");
            };
            assert_eq!(calls[0].name, name);
            assert_eq!(calls[0].arguments, arguments);
            let ProjectedContent::ToolResult(returned) = &projected.request.history[1].content
            else {
                panic!("expected tool result");
            };
            assert_eq!(returned.name, name);
            assert_eq!(returned.call_id, "call-1");
            assert_eq!(
                serde_json::from_str::<serde_json::Value>(&returned.content).unwrap(),
                result
            );
        }
    }

    #[test]
    fn preserves_provider_state_through_cursor_wire_and_tool_round_trip() {
        for (kind, value) in [
            ("openai_chat", json!({"reasoning_content":"thought"})),
            (
                "openai_responses",
                json!({"items":[{"type":"reasoning","id":"rs_1",
                "summary":[],"encrypted_content":"opaque"}]}),
            ),
            (
                "anthropic",
                json!({"blocks":[{"type":"thinking","thinking":"thought",
                "signature":"signed"}]}),
            ),
        ] {
            let state = ProviderReplayState {
                provider_kind: kind.into(),
                value,
            };
            let mut output = InvocationOutput::default();
            for event in [
                ModelEvent::ThinkingDelta("thought".into()),
                ModelEvent::ThinkingEnd,
                ModelEvent::ToolCallStart {
                    index: 0,
                    call_id: "call-1".into(),
                    name: "Read".into(),
                },
                ModelEvent::ToolCallArgumentsDelta {
                    index: 0,
                    delta: r#"{"path":"a.txt"}"#.into(),
                },
                ModelEvent::ToolCallEnd { index: 0 },
                ModelEvent::ProviderReplayState(state.clone()),
            ] {
                output.event(event, "model-hash").unwrap();
            }
            let responses = output
                .event(ModelEvent::Done(FinishReason::ToolUse), "model-hash")
                .unwrap();
            let wire = responses[0].encode_to_vec();
            let wire = pb::InferenceStreamResponse::decode(wire.as_slice()).unwrap();
            let Some(pb::inference_stream_response::Response::ResponseInfo(info)) = wire.response
            else {
                panic!("expected response info");
            };
            let assistant = info.messages.into_iter().next().unwrap();
            let request = pb::InferenceStreamRequest {
                messages: vec![pb::InferenceCoreMessage {
                    role: 2,
                    tool_calls: assistant.tool_calls,
                    reasoning_parts: assistant.reasoning_parts,
                    anthropic_native_content: assistant.anthropic_native_content,
                    content: assistant
                        .content
                        .map(pb::inference_core_message::Content::Text),
                    ..Default::default()
                }],
                ..Default::default()
            };
            let call = invocation(
                &request,
                "model-hash",
                "conversation",
                "run",
                "invocation",
                0,
            )
            .unwrap();
            let ProjectedContent::Assistant {
                replay_state,
                calls,
                ..
            } = &call.request.history[0].content
            else {
                panic!("expected assistant");
            };
            assert_eq!(replay_state.as_ref(), Some(&state), "{kind}");
            assert_eq!(calls[0].arguments["path"], "a.txt", "{kind}");
        }
    }
}
