//! Verifies Cursor's binary Bidi payloads reach the Agent stream and its KV replies.
#[path = "support/fake_provider.rs"]
mod fake_provider;
#[path = "support/fixtures.rs"]
mod fixtures;

use std::{sync::Arc, time::Duration};

use axum::{
    body::{to_bytes, Body},
    http::Request,
    Router,
};
use cursor_server::{
    api::cursor::{self, bidi},
    cursor::{
        prompting::{PromptAssets, PromptCompiler},
        protocol::{
            connect,
            proto::{agent::v1 as pb, aiserver::v1 as ai},
        },
        transport::TransportRegistry,
    },
    model::ModelConfigInput,
    network::NetworkClients,
    provider::{FinishReason, ModelEvent},
};
use futures_util::StreamExt;
use prost::Message;
use tower::ServiceExt;

fn wire(message: pb::AgentClientMessage, seqno: i64, binary: bool) -> ai::BidiAppendRequest {
    let bytes = message.encode_to_vec();
    ai::BidiAppendRequest {
        request_id: Some(ai::BidiRequestId {
            request_id: "binary-agent".into(),
        }),
        append_seqno: seqno,
        data: if binary {
            String::new()
        } else {
            hex::encode(&bytes)
        },
        data_binary: if binary { bytes } else { Vec::new() },
    }
}

#[test]
fn binary_and_hex_preserve_every_agent_message_variant_and_sequence() {
    use pb::agent_client_message::Message as M;
    let messages = [
        M::RunRequest(Default::default()),
        M::ExecClientMessage(Default::default()),
        M::ExecClientControlMessage(Default::default()),
        M::KvClientMessage(Default::default()),
        M::ConversationAction(Default::default()),
        M::InteractionResponse(Default::default()),
        M::ClientHeartbeat(Default::default()),
        M::PrewarmRequest(Default::default()),
    ];
    for (seqno, message) in messages.into_iter().enumerate() {
        let message = pb::AgentClientMessage {
            message: Some(message),
        };
        for binary in [false, true] {
            let decoded = bidi::decode(&wire(message.clone(), seqno as i64, binary)).unwrap();
            assert_eq!(decoded.request_id, "binary-agent");
            assert_eq!(decoded.seqno, seqno as i64);
            assert_eq!(decoded.message, message);
        }
    }
}

#[test]
fn binary_payload_takes_precedence_over_hex() {
    let message = pb::AgentClientMessage {
        message: Some(pb::agent_client_message::Message::ClientHeartbeat(
            Default::default(),
        )),
    };
    let mut request = wire(message.clone(), 7, true);
    request.data = "not hex".into();
    assert_eq!(bidi::decode(&request).unwrap().message, message);
}

#[test]
fn malformed_binary_is_rejected_even_when_hex_is_valid() {
    let message = pb::AgentClientMessage {
        message: Some(pb::agent_client_message::Message::ClientHeartbeat(
            Default::default(),
        )),
    };
    let mut request = wire(message, 0, false);
    request.data_binary = vec![0xff];
    assert!(bidi::decode(&request).is_err());
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

async fn append(app: &Router, message: pb::AgentClientMessage, seqno: i64, binary: bool) {
    let response = post(
        app,
        "/aiserver.v1.BidiService/BidiAppend",
        &wire(message, seqno, binary),
    )
    .await;
    let status = response.status();
    let body = to_bytes(response.into_body(), 4096).await.unwrap();
    assert_eq!(status, 200, "{}", String::from_utf8_lossy(&body));
}

async fn round_trip(binary: bool) {
    let (_dir, store) = fixtures::temp_store().await;
    let config: ModelConfigInput = serde_json::from_value(serde_json::json!({
        "display_name": "Binary fixture", "type": "openai",
        "base_url": "https://example.com/v1", "api_key": "test-key",
        "tooltip_data": "Test", "model_id": "test-model",
    }))
    .unwrap();
    let model = store.create_model(&config).await.unwrap();
    let provider = Arc::new(fake_provider::FakeProvider::default());
    provider.push(vec![
        ModelEvent::Start {
            model_call_id: "binary-call".into(),
        },
        ModelEvent::TextStart,
        ModelEvent::TextDelta("BINARY_OK".into()),
        ModelEvent::TextEnd,
        ModelEvent::Done(FinishReason::Stop),
    ]);
    let assets =
        PromptAssets::load(&std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("prompt/cursor"))
            .unwrap();
    let registry =
        TransportRegistry::new(store.clone(), provider.clone(), PromptCompiler::new(assets));
    let app = cursor::router(registry, NetworkClients::new(store)).unwrap();
    let run = pb::AgentClientMessage {
        message: Some(pb::agent_client_message::Message::RunRequest(
            pb::AgentRunRequest {
                action: Some(pb::ConversationAction {
                    action: Some(pb::conversation_action::Action::UserMessageAction(
                        pb::UserMessageAction {
                            user_message: Some(pb::UserMessage {
                                text: "Reply BINARY_OK".into(),
                                message_id: "binary-user".into(),
                                mode: pb::AgentMode::Agent as i32,
                                ..Default::default()
                            }),
                            ..Default::default()
                        },
                    )),
                    ..Default::default()
                }),
                conversation_id: Some("binary-conversation".into()),
                requested_model: Some(pb::RequestedModel {
                    model_id: model.model_hash,
                    ..Default::default()
                }),
                ..Default::default()
            },
        )),
    };
    append(&app, run, 0, binary).await;
    let response = post(
        &app,
        "/agent.v1.AgentService/RunSSE",
        &pb::BidiRequestId {
            request_id: "binary-agent".into(),
        },
    )
    .await;
    assert_eq!(response.status(), 200);
    let mut stream = response.into_body().into_data_stream();
    let (mut seqno, mut text, mut turn_ended, mut kv_acks) = (1, String::new(), false, 0);
    loop {
        let chunk = tokio::time::timeout(Duration::from_secs(5), stream.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        for (flags, payload) in connect::decode_frames(&chunk).unwrap() {
            if flags & connect::END_STREAM_FLAG != 0 {
                assert_eq!(
                    serde_json::from_slice::<serde_json::Value>(&payload).unwrap(),
                    serde_json::json!({})
                );
                assert!(turn_ended);
                assert!(kv_acks > 0);
                assert_eq!(text, "BINARY_OK");
                assert_eq!(provider.requests().len(), 1);
                return;
            }
            match pb::AgentServerMessage::decode(payload).unwrap().message {
                Some(pb::agent_server_message::Message::KvServerMessage(kv)) => {
                    append(
                        &app,
                        pb::AgentClientMessage {
                            message: Some(pb::agent_client_message::Message::KvClientMessage(
                                pb::KvClientMessage {
                                    id: kv.id,
                                    message: Some(pb::kv_client_message::Message::SetBlobResult(
                                        pb::SetBlobResult { error: None },
                                    )),
                                },
                            )),
                        },
                        seqno,
                        binary,
                    )
                    .await;
                    seqno += 1;
                    kv_acks += 1;
                }
                Some(pb::agent_server_message::Message::InteractionUpdate(update)) => {
                    match update.message {
                        Some(pb::interaction_update::Message::TextDelta(delta)) => {
                            text.push_str(&delta.text)
                        }
                        Some(pb::interaction_update::Message::TurnEnded(_)) => turn_ended = true,
                        _ => {}
                    }
                }
                _ => {}
            }
        }
    }
}

#[tokio::test]
async fn binary_http_append_and_kv_replies_reach_text_and_turn_end() {
    round_trip(true).await;
}

#[tokio::test]
async fn hex_http_append_and_kv_replies_reach_text_and_turn_end() {
    round_trip(false).await;
}
