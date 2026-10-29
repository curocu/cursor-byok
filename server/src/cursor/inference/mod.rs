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
