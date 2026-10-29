//! Exercises the actual HTTP/1 Bidi + RunInferenceSSE contract used by Cursor 3.27.22.
#[path = "support/fake_provider.rs"]
mod fake_provider;
#[path = "support/fixtures.rs"]
mod fixtures;

use axum::{body::Body, http::Request, Router};
use cursor_server::{
    api::cursor,
    cursor::{
        prompting::{PromptAssets, PromptCompiler},
        protocol::{
            connect,
            proto::{agent::v1 as agent, aiserver::v1 as ai, inference as pb},
        },
        transport::TransportRegistry,
    },
    model::{ModelConfigInput, ModelType, ProjectedContent, OPENAI_CHAT_ENDPOINT},
    network::NetworkClients,
    provider::{FinishReason, ModelEvent},
};
use futures_util::{Stream, StreamExt};
use prost::Message;
use std::{sync::Arc, time::Duration};
use tower::ServiceExt;

fn model_input() -> ModelConfigInput {
    ModelConfigInput {
        sort_order: 1,
        display_name: "Inference test".into(),
        group_name: None,
        model_type: ModelType::OpenAi,
        base_url: "https://example.com/v1".into(),
        use_full_url: false,
        api_key: "test-key".into(),
        tooltip_data: "Test".into(),
        model_id: "test-model".into(),
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
    }
}

async fn post(app: &Router, path: &str, message: &impl Message) -> axum::response::Response {
    app.clone()
        .oneshot(
            Request::post(path)
                .header("content-type", "application/proto")
                .body(Body::from(message.encode_to_vec()))
                .unwrap(),
        )
        .await
        .unwrap()
}

async fn append(
    app: &Router,
    id: &str,
    seq: i64,
    message: pb::run_inference_client_message::Message,
) {
    let message = pb::RunInferenceClientMessage {
        message: Some(message),
    };
    let response = post(
        app,
        "/aiserver.v1.BidiService/BidiAppend",
        &ai::BidiAppendRequest {
            request_id: Some(ai::BidiRequestId {
                request_id: id.into(),
            }),
            append_seqno: seq,
            data: hex::encode(message.encode_to_vec()),
            ..Default::default()
        },
    )
    .await;
    let status = response.status();
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    assert_eq!(status, 200, "{}", String::from_utf8_lossy(&body));
}

async fn next<S>(stream: &mut S) -> pb::run_inference_server_message::Message
where
    S: Stream<Item = std::result::Result<bytes::Bytes, axum::Error>> + Unpin,
{
    loop {
        let frame = tokio::time::timeout(Duration::from_secs(5), stream.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        let (flags, payload) = connect::decode_frames(&frame).unwrap().pop().unwrap();
        assert_eq!(flags, 0, "unexpected terminal frame: {:?}", payload);
        let message = pb::RunInferenceServerMessage::decode(payload)
            .unwrap()
            .message
            .unwrap();
        if matches!(
            message,
            pb::run_inference_server_message::Message::Heartbeat(_)
        ) {
            continue;
        }
        return message;
    }
}

#[tokio::test]
async fn cursor_local_loop_can_call_a_tool_and_submit_its_result() {
    use pb::run_inference_client_message::Message as C;
    use pb::run_inference_server_message::Message as S;
    let (_dir, store) = fixtures::temp_store().await;
    let model = store.create_model(&model_input()).await.unwrap();
    let provider = Arc::new(fake_provider::FakeProvider::default());
    provider.push(vec![
        ModelEvent::TextDelta("Reading file".into()),
        ModelEvent::TextEnd,
        ModelEvent::ToolCallStart {
            index: 0,
            call_id: "tool-1".into(),
            name: "Read".into(),
        },
        ModelEvent::ToolCallArgumentsDelta {
            index: 0,
            delta: "{\"path\":\"fixture.txt\"}".into(),
        },
        ModelEvent::ToolCallEnd { index: 0 },
        ModelEvent::Done(FinishReason::ToolUse),
    ]);
    provider.push(vec![
        ModelEvent::TextDelta("The file says hello".into()),
        ModelEvent::Done(FinishReason::Stop),
    ]);
    provider.push_pending();
    let assets =
        PromptAssets::load(&std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("prompt/cursor"))
            .unwrap();
    let registry =
        TransportRegistry::new(store.clone(), provider.clone(), PromptCompiler::new(assets));
    let app = cursor::router(registry, NetworkClients::new(store)).unwrap();
    let id = "6e8d672f-2928-486b-ad47-400ea497175e";
    append(
        &app,
        id,
        0,
        C::RunRequest(pb::RunInferenceRunRequest {
            conversation_id: "21dd4a99-3ad4-4f34-9b2a-49c9335df741".into(),
            turn_agent_mode: Some("multitask".into()),
            requested_model: Some(pb::InferenceRequestedModel {
                model_id: model.model_hash.clone(),
                ..Default::default()
            }),
            ..Default::default()
        }),
    )
    .await;
    let response = post(
        &app,
        "/aiserver.v1.InferenceService/RunInferenceSSE",
        &agent::BidiRequestId {
            request_id: id.into(),
        },
    )
    .await;
    assert_eq!(response.status(), 200);
    let mut stream = response.into_body().into_data_stream();
    let S::RunReady(ready) = next(&mut stream).await else {
        panic!("expected run_ready")
    };
    assert!(ready.accepts_agent_events);
    let metadata = ready.prompt_model_metadata.unwrap();
    assert_eq!(
        metadata.feature_flags["dynamicToolProfile"].value,
        Some(pb::run_inference_flag_value::Value::StringValue(
            "all-static".into()
        ))
    );
    for mode in ["agent", "chat", "plan", "debug", "triage", "multitask"] {
        assert!(metadata
            .agent_mode_configs
            .iter()
            .any(|m| m.mode_id == mode));
    }
    let subagents = metadata.subagent_model_config.unwrap();
    assert_eq!(subagents.force_policy, "none");
    assert!(subagents
        .models
        .iter()
        .any(|m| m.slug == model.model_hash && m.valid));
    let input = pb::InferenceStreamRequest {
        tools: vec![pb::InferenceAgentTool {
            name: "Read".into(),
            parameters: Some(prost_types::Struct {
                fields: std::collections::BTreeMap::from([(
                    "jsonSchema".into(),
                    prost_types::Value {
                        kind: Some(prost_types::value::Kind::StructValue(prost_types::Struct {
                            fields: std::collections::BTreeMap::from([(
                                "type".into(),
                                prost_types::Value {
                                    kind: Some(prost_types::value::Kind::StringValue(
                                        "object".into(),
                                    )),
                                },
                            )]),
                        })),
                    },
                )]),
            }),
            ..Default::default()
        }],
        messages: vec![pb::InferenceCoreMessage {
            role: 1,
            content: Some(pb::inference_core_message::Content::Text(
                "Read fixture.txt".into(),
            )),
            ..Default::default()
        }],
        ..Default::default()
    };
    append(
        &app,
        id,
        1,
        C::InvokeModel(pb::RunInferenceInvokeModel {
            invocation_id: "call-1".into(),
            request: Some(input.clone()),
            ..Default::default()
        }),
    )
    .await;
    // Retried appends must not invoke a provider twice.
    append(
        &app,
        id,
        1,
        C::ClientHeartbeat(pb::RunInferenceClientHeartbeat::default()),
    )
    .await;
    let mut assistant = None;
    let mut completed_args = None;
    let mut invocation_id = None;
    loop {
        match next(&mut stream).await {
            S::InvocationResponse(r) => match r.response.unwrap().response.unwrap() {
                pb::inference_stream_response::Response::ToolCallPart(p) if p.is_complete => {
                    completed_args = Some(p.args);
                }
                pb::inference_stream_response::Response::InvocationId(info) => {
                    invocation_id = Some(info.invocation_id);
                }
                pb::inference_stream_response::Response::ResponseInfo(info) => {
                    assert!(info.created_at > 1_000_000_000_000);
                    assistant = info.messages.into_iter().next()
                }
                _ => {}
            },
            S::InvocationEnd(end) => {
                assert!(end.error.is_none());
                break;
            }
            _ => {}
        }
    }
    assert_eq!(
        completed_args.as_deref(),
        Some("{\"path\":\"fixture.txt\"}")
    );
    assert_eq!(invocation_id.as_deref(), Some("call-1"));
    let assistant = assistant.unwrap();
    let mut input = input;
    input.messages.push(pb::InferenceCoreMessage {
        role: 2,
        tool_calls: assistant.tool_calls,
        content: assistant
            .content
            .map(pb::inference_core_message::Content::Text),
        ..Default::default()
    });
    input.messages.push(pb::InferenceCoreMessage {
        role: 3,
        content: Some(pb::inference_core_message::Content::ToolContent(
            pb::InferenceToolResultContent {
                parts: vec![pb::InferenceToolResultPart {
                    tool_call_id: "tool-1".into(),
                    tool_name: "Read".into(),
                    result: Some(prost_types::Value {
                        kind: Some(prost_types::value::Kind::StringValue("hello".into())),
                    }),
                    ..Default::default()
                }],
            },
        )),
        ..Default::default()
    });
    append(
        &app,
        id,
        2,
        C::InvokeModel(pb::RunInferenceInvokeModel {
            invocation_id: "call-2".into(),
            request: Some(input),
            ..Default::default()
        }),
    )
    .await;
    loop {
        if let S::InvocationEnd(end) = next(&mut stream).await {
            assert!(end.error.is_none());
            break;
        }
    }
    let requests = provider.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0].prompt.tools[0].parameters["type"], "object");
    assert!(requests[0].prompt.tools[0]
        .parameters
        .get("jsonSchema")
        .is_none());
    assert!(
        matches!(&requests[1].history[2].content, ProjectedContent::ToolResult(r) if r.content=="hello" && r.call_id=="tool-1")
    );
    append(
        &app,
        id,
        3,
        C::InvokeModel(pb::RunInferenceInvokeModel {
            invocation_id: "cancel-me".into(),
            request: Some(pb::InferenceStreamRequest::default()),
            ..Default::default()
        }),
    )
    .await;
    append(
        &app,
        id,
        4,
        C::CancelInvocation(pb::RunInferenceCancelInvocation {
            invocation_id: "cancel-me".into(),
        }),
    )
    .await;
    loop {
        if let S::InvocationEnd(end) = next(&mut stream).await {
            assert_eq!(end.invocation_id, "cancel-me");
            assert_eq!(end.error.unwrap().code, 1);
            break;
        }
    }
    append(
        &app,
        id,
        5,
        C::FinishRun(pb::RunInferenceFinishRun::default()),
    )
    .await;
}

#[test]
fn new_run_schema_reproduces_the_reported_old_decoder_failure() {
    let message = pb::RunInferenceClientMessage {
        message: Some(pb::run_inference_client_message::Message::RunRequest(
            pb::RunInferenceRunRequest {
                conversation_id: "21dd4a99-3ad4-4f34-9b2a-49c9335df741".into(),
                ..Default::default()
            },
        )),
    };
    let bytes = message.encode_to_vec();
    assert!(agent::AgentClientMessage::decode(bytes.as_slice()).is_err());
    assert!(pb::RunInferenceClientMessage::decode(bytes.as_slice()).is_ok());
}

#[tokio::test]
async fn labeling_records_for_byok_stay_local_while_official_models_are_forwarded() {
    use cursor_server::cursor::services::inference_telemetry::{FOLLOWUP_PATH, POST_TURN_PATH};
    let (_dir, store) = fixtures::temp_store().await;
    let model = store.create_model(&model_input()).await.unwrap();
    let assets =
        PromptAssets::load(&std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("prompt/cursor"))
            .unwrap();
    let registry = TransportRegistry::new(
        store.clone(),
        Arc::new(fake_provider::FakeProvider::default()),
        PromptCompiler::new(assets),
    );
    let app = cursor::router(registry, NetworkClients::new(store)).unwrap();
    for (path, model_tag) in [(FOLLOWUP_PATH, 6), (POST_TURN_PATH, 5)] {
        for selected in [
            model.model_hash.as_str(),
            "plugin:test:model",
            "official-model",
        ] {
            // An invalid official upstream URL makes accidental forwarding fail locally.
            let mut data = vec![(model_tag << 3) | 2, selected.len() as u8];
            data.extend_from_slice(selected.as_bytes());
            let response = app
                .clone()
                .oneshot(
                    Request::post(path)
                        .header("content-type", "application/proto")
                        .header("x-server-upstream-url", "https://example.com/never-forward")
                        .body(Body::from(data))
                        .unwrap(),
                )
                .await
                .unwrap();
            let status = response.status();
            let body = axum::body::to_bytes(response.into_body(), usize::MAX)
                .await
                .unwrap();
            if selected == "official-model" {
                assert_eq!(status, 400);
                assert!(String::from_utf8_lossy(&body)
                    .contains("upstream URL must target a Cursor HTTPS host"));
            } else {
                assert_eq!(status, 200, "{}", String::from_utf8_lossy(&body));
                assert!(body.is_empty());
            }
        }
    }
}
