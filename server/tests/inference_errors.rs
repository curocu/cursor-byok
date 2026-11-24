//! Upstream HTTP failures through the actual Cursor Bidi/SSE invocation path.
#[path = "support/fixtures.rs"]
mod fixtures;

use axum::{
    body::Body,
    http::{Request, StatusCode},
    routing::post,
    Router,
};
use cursor_server::{
    api::cursor,
    config::{ProviderConfig, ProviderKind},
    cursor::{
        prompting::{PromptAssets, PromptCompiler},
        protocol::{
            connect,
            proto::{agent::v1 as agent, aiserver::v1 as ai, inference as pb},
        },
        transport::TransportRegistry,
    },
    model::{ModelConfigInput, ModelType, OPENAI_CHAT_ENDPOINT},
    network::NetworkClients,
    provider::build_provider,
};
use futures_util::StreamExt;
use prost::Message;
use std::{
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    time::Duration,
};
use tower::ServiceExt;

#[derive(Clone, PartialEq, Message)]
struct ErrorDetailsOnWire {
    #[prost(int32, tag = "1")]
    error: i32,
    #[prost(message, optional, tag = "2")]
    details: Option<CustomDetailsOnWire>,
}

#[derive(Clone, PartialEq, Message)]
struct CustomDetailsOnWire {
    #[prost(string, tag = "2")]
    detail: String,
    #[prost(bool, optional, tag = "4")]
    is_retryable: Option<bool>,
    #[prost(map = "string, string", tag = "7")]
    additional_info: std::collections::HashMap<String, String>,
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
    let response = app
        .clone()
        .oneshot(
            Request::post("/aiserver.v1.BidiService/BidiAppend")
                .body(Body::from(
                    ai::BidiAppendRequest {
                        request_id: Some(ai::BidiRequestId {
                            request_id: id.into(),
                        }),
                        append_seqno: seq,
                        data_binary: message.encode_to_vec(),
                        ..Default::default()
                    }
                    .encode_to_vec(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
}

#[tokio::test]
async fn upstream_http_errors_keep_status_retry_guidance_and_native_classification() {
    use pb::run_inference_client_message::Message as C;
    use pb::run_inference_server_message::Message as S;
    for kind in [
        ProviderKind::OpenAiChat,
        ProviderKind::OpenAiResponses,
        ProviderKind::Anthropic,
    ] {
        for (status, code, retryable) in [
            (429, 8, true),
            (401, 16, false),
            (403, 7, false),
            (400, 3, false),
            (404, 5, false),
            (503, 14, true),
            (529, 14, true),
        ] {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap();
            let requests = Arc::new(AtomicUsize::new(0));
            let received = requests.clone();
            let success = match kind {
                ProviderKind::OpenAiChat => "data: {\"choices\":[{\"delta\":{\"content\":\"OK\"},\"finish_reason\":\"stop\"}]}\n\n",
                ProviderKind::OpenAiResponses => "data: {\"type\":\"response.output_text.delta\",\"item_id\":\"m\",\"output_index\":0,\"content_index\":0,\"delta\":\"OK\"}\n\ndata: {\"type\":\"response.completed\",\"response\":{\"output\":[]}}\n\n",
                ProviderKind::Anthropic => "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"OK\"}}\n\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"}}\n\ndata: {\"type\":\"message_stop\"}\n\n",
            };
            let mock = Router::new().route("/", post(move || {
                let received = received.clone();
                async move {
                    if received.fetch_add(1, Ordering::SeqCst) == 0 {
                        (StatusCode::from_u16(status).unwrap(), [("content-type", "application/json"), ("retry-after", "7")],
                            r#"{"error":{"message":"synthetic failure test-upstream-secret"},"input":"PRIVATE_PAYLOAD_MUST_NOT_ESCAPE"}"#)
                    } else {
                        (StatusCode::OK, [("content-type", "text/event-stream"), ("retry-after", "0")], success)
                    }
                }
            }));
            let server = tokio::spawn(async move { axum::serve(listener, mock).await.unwrap() });
            let provider = build_provider(&ProviderConfig {
                kind: kind.clone(),
                request_url: format!("http://{addr}/"),
                api_key: "test-upstream-secret".into(),
                custom_headers: Default::default(),
                max_output_tokens: Some(128),
                request_timeout: Duration::from_secs(5),
                allowed_body_fields: None,
            })
            .unwrap();
            let (_dir, store) = fixtures::temp_store().await;
            let model = store
                .create_model(&ModelConfigInput {
                    sort_order: 0,
                    display_name: "Error test".into(),
                    group_name: None,
                    model_type: ModelType::OpenAi,
                    base_url: "https://example.com/v1".into(),
                    use_full_url: false,
                    api_key: "unused".into(),
                    tooltip_data: "Error test".into(),
                    model_id: "error-test".into(),
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
                })
                .await
                .unwrap();
            let assets = PromptAssets::load(
                &std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("prompt/cursor"),
            )
            .unwrap();
            let registry = TransportRegistry::new(
                store.clone(),
                Arc::clone(&provider),
                PromptCompiler::new(assets),
            );
            let app = cursor::router(registry, NetworkClients::new(store)).unwrap();
            let id = format!("http-error-{kind:?}-{status}");
            append(
                &app,
                &id,
                0,
                C::RunRequest(pb::RunInferenceRunRequest {
                    conversation_id: id.clone(),
                    requested_model: Some(pb::InferenceRequestedModel {
                        model_id: model.model_hash,
                        ..Default::default()
                    }),
                    ..Default::default()
                }),
            )
            .await;
            let response = app
                .clone()
                .oneshot(
                    Request::post("/aiserver.v1.InferenceService/RunInferenceSSE")
                        .body(Body::from(
                            agent::BidiRequestId {
                                request_id: id.clone(),
                            }
                            .encode_to_vec(),
                        ))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), 200);
            let mut stream = response.into_body().into_data_stream();
            append(
                &app,
                &id,
                1,
                C::InvokeModel(pb::RunInferenceInvokeModel {
                    invocation_id: "failed-sample".into(),
                    request: Some(pb::InferenceStreamRequest {
                        messages: vec![pb::InferenceCoreMessage {
                            role: 1,
                            content: Some(pb::inference_core_message::Content::Text(
                                "synthetic prompt".into(),
                            )),
                            ..Default::default()
                        }],
                        ..Default::default()
                    }),
                    ..Default::default()
                }),
            )
            .await;
            let error = loop {
                let frame = tokio::time::timeout(Duration::from_secs(5), stream.next())
                    .await
                    .unwrap()
                    .unwrap()
                    .unwrap();
                let mut found = None;
                for (flags, bytes) in connect::decode_frames(&frame).unwrap() {
                    assert_eq!(flags, 0);
                    if let Some(S::InvocationEnd(end)) =
                        pb::RunInferenceServerMessage::decode(bytes)
                            .unwrap()
                            .message
                    {
                        found = end.error;
                    }
                }
                if let Some(e) = found {
                    break e;
                }
            };
            assert_eq!(
                error.code, code,
                "{kind:?}: HTTP {status} lost its native classification"
            );
            assert!(error.message.contains(&status.to_string()));
            assert!(error.message.contains("synthetic failure"));
            assert!(!error.message.contains("test-upstream-secret"));
            assert!(!error.message.contains("PRIVATE_PAYLOAD_MUST_NOT_ESCAPE"));
            let detail = error
                .details
                .iter()
                .find(|d| d.r#type == "aiserver.v1.ErrorDetails")
                .expect("native error details");
            let detail = ErrorDetailsOnWire::decode(detail.value.as_slice()).unwrap();
            assert_eq!(detail.error, if status == 401 { 42 } else { 57 });
            let custom = detail.details.unwrap();
            assert_eq!(custom.is_retryable, Some(retryable));
            assert_eq!(
                custom
                    .additional_info
                    .get("upstreamHttpStatus")
                    .map(String::as_str),
                Some(status.to_string().as_str())
            );
            assert_eq!(
                custom
                    .additional_info
                    .get("upstreamRetryAfter")
                    .map(String::as_str),
                Some("7")
            );
            assert!(custom.detail.contains("Retry-After: 7"));
            println!(
                "NATIVE_HTTP_ERROR {}",
                serde_json::json!({
                    "provider": format!("{kind:?}"), "http_status": status, "code": error.code, "message": error.message,
                    "details": error.details.iter().map(|d| serde_json::json!({"type": d.r#type, "hex": hex::encode(&d.value)})).collect::<Vec<_>>()
                })
            );
            assert_eq!(
                requests.load(Ordering::SeqCst),
                1,
                "BYOK must not retry on its own"
            );
            append(
                &app,
                &id,
                2,
                C::InvokeModel(pb::RunInferenceInvokeModel {
                    invocation_id: "recovered-sample".into(),
                    request: Some(pb::InferenceStreamRequest::default()),
                    ..Default::default()
                }),
            )
            .await;
            let mut recovered = String::new();
            'recovery: loop {
                let frame = tokio::time::timeout(Duration::from_secs(5), stream.next())
                    .await
                    .unwrap()
                    .unwrap()
                    .unwrap();
                for (flags, bytes) in connect::decode_frames(&frame).unwrap() {
                    assert_eq!(flags, 0);
                    match pb::RunInferenceServerMessage::decode(bytes)
                        .unwrap()
                        .message
                    {
                        Some(S::InvocationResponse(r)) => {
                            if let Some(pb::inference_stream_response::Response::TextPart(t)) =
                                r.response.and_then(|r| r.response)
                            {
                                recovered.push_str(&t.text)
                            }
                        }
                        Some(S::InvocationEnd(end)) => {
                            assert!(end.error.is_none(), "recovery failed: {:?}", end.error);
                            break 'recovery;
                        }
                        _ => {}
                    }
                }
            }
            assert_eq!(recovered, "OK");
            assert_eq!(requests.load(Ordering::SeqCst), 2);
            append(
                &app,
                &id,
                3,
                C::FinishRun(pb::RunInferenceFinishRun::default()),
            )
            .await;
            drop(stream);
            server.abort();
        }
    }
}

#[tokio::test]
async fn transport_failures_reach_cursor_and_a_new_invocation_can_recover() {
    use cursor_server::provider::ProviderRouter;
    use pb::run_inference_client_message::Message as C;
    use pb::run_inference_server_message::Message as S;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    for (model_type, endpoint, partial, success) in [
        (ModelType::OpenAi, OPENAI_CHAT_ENDPOINT,
            "data: {\"choices\":[{\"delta\":{\"content\":\"PARTIAL\"}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"content\":\"OK\"},\"finish_reason\":\"stop\"}]}\n\n"),
        (ModelType::OpenAi, cursor_server::model::OPENAI_RESPONSES_ENDPOINT,
            "data: {\"type\":\"response.output_text.delta\",\"item_id\":\"m\",\"output_index\":0,\"content_index\":0,\"delta\":\"PARTIAL\"}\n\n",
            "data: {\"type\":\"response.output_text.delta\",\"item_id\":\"m\",\"output_index\":0,\"content_index\":0,\"delta\":\"OK\"}\n\ndata: {\"type\":\"response.completed\",\"response\":{\"output\":[]}}\n\n"),
        (ModelType::Anthropic, OPENAI_CHAT_ENDPOINT,
            "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"PARTIAL\"}}\n\n",
            "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"OK\"}}\n\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"}}\n\ndata: {\"type\":\"message_stop\"}\n\n"),
    ] {
        for failure in ["truncated", "invalid-gzip", "timeout"] {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let requests = Arc::new(AtomicUsize::new(0));
            let received = requests.clone();
            let server = tokio::spawn(async move {
                loop {
                    let (mut socket, _) = listener.accept().await.unwrap();
                    let first = received.fetch_add(1, Ordering::SeqCst) == 0;
                    tokio::spawn(async move {
                        let mut request = Vec::new();
                        let mut buffer = [0_u8; 4096];
                        loop {
                            let count = socket.read(&mut buffer).await.unwrap();
                            if count == 0 { return; }
                            request.extend_from_slice(&buffer[..count]);
                            if let Some(end) = request.windows(4).position(|w| w == b"\r\n\r\n") {
                                let headers = String::from_utf8_lossy(&request[..end]);
                                let length: usize = headers.lines().find_map(|line| line.to_ascii_lowercase().strip_prefix("content-length:").map(|v| v.trim().parse().unwrap())).unwrap_or(0);
                                if request.len() >= end + 4 + length { break; }
                            }
                        }
                        if first && failure == "timeout" {
                            tokio::time::sleep(Duration::from_secs(1)).await;
                            return;
                        }
                        let response = if !first {
                            format!("HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{success}", success.len())
                        } else if failure == "truncated" {
                            format!("HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: 99999\r\nConnection: close\r\n\r\n{partial}")
                        } else {
                            "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Encoding: gzip\r\nContent-Length: 8\r\nConnection: close\r\n\r\nnot-gzip".into()
                        };
                        socket.write_all(response.as_bytes()).await.unwrap();
                    });
                }
            });
            let (_directory, store) = fixtures::temp_store().await;
            let model = store.create_model(&ModelConfigInput {
                sort_order: 0, display_name: "Transport test".into(), group_name: None,
                model_type, base_url: format!("http://{address}/?key=synthetic-url-secret"), use_full_url: true,
                api_key: "synthetic-header-secret".into(), tooltip_data: "Transport test".into(),
                model_id: "transport-test".into(), reasoning_effort: None,
                openai_endpoint: endpoint.into(), openai_extra_params_enabled: false,
                openai_extra_params: serde_json::json!({}), custom_headers_enabled: false,
                custom_headers: serde_json::json!({}), anthropic_extra_params_enabled: false,
                anthropic_extra_params: serde_json::json!({}), context_window_tokens: None,
                max_completion_tokens: None, anthropic_max_tokens: None,
                anthropic_thinking_effort: None, thinking_budget_tokens: None,
            }).await.unwrap();
            let clients = NetworkClients::new(store.clone());
            let limit = Duration::from_millis(200);
            let provider = Arc::new(ProviderRouter::new(store.clone(), clients.clone(), limit, Duration::from_secs(5)));
            let assets = PromptAssets::load(&std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("prompt/cursor")).unwrap();
            let registry = TransportRegistry::new(store, provider, PromptCompiler::new(assets));
            let app = cursor::router(registry, clients).unwrap();
            let id = format!("transport-{endpoint}-{failure}");
            append(&app, &id, 0, C::RunRequest(pb::RunInferenceRunRequest {
                conversation_id: id.clone(), requested_model: Some(pb::InferenceRequestedModel {
                    model_id: model.model_hash, ..Default::default()
                }), ..Default::default()
            })).await;
            let response = app.clone().oneshot(Request::post("/aiserver.v1.InferenceService/RunInferenceSSE")
                .body(Body::from(agent::BidiRequestId { request_id: id.clone() }.encode_to_vec())).unwrap()).await.unwrap();
            let mut stream = response.into_body().into_data_stream();
            for (sequence, invocation_id) in [(1, "failed"), (2, "recovered")] {
                append(&app, &id, sequence, C::InvokeModel(pb::RunInferenceInvokeModel {
                    invocation_id: invocation_id.into(), request: Some(pb::InferenceStreamRequest::default()), ..Default::default()
                })).await;
                let mut text = String::new();
                'invocation: loop {
                    let frame = tokio::time::timeout(Duration::from_secs(5), stream.next()).await.unwrap().unwrap().unwrap();
                    for (flags, bytes) in connect::decode_frames(&frame).unwrap() {
                        assert_eq!(flags, 0);
                        match pb::RunInferenceServerMessage::decode(bytes).unwrap().message {
                            Some(S::InvocationResponse(r)) => {
                                assert_eq!(r.invocation_id, invocation_id);
                                if let Some(pb::inference_stream_response::Response::TextPart(t)) = r.response.and_then(|r| r.response) { text.push_str(&t.text); }
                            }
                            Some(S::InvocationEnd(end)) => {
                                assert_eq!(end.invocation_id, invocation_id);
                                if invocation_id == "failed" {
                                    let error = end.error.expect("failed transport must not look like successful completion");
                                    assert_eq!(error.code, 13);
                                    assert!(error.message.contains(if failure == "timeout" { "request timed out after" } else { "while reading the response body" }), "{}", error.message);
                                    assert!(!error.message.contains("synthetic-url-secret"));
                                    assert!(!error.message.contains("synthetic-header-secret"));
                                    if failure == "timeout" { assert!(error.message.contains("request timeout limit: 200 ms")); }
                                    assert_eq!(text, if failure == "truncated" { "PARTIAL" } else { "" });
                                } else {
                                    assert!(end.error.is_none(), "{:?}", end.error);
                                    assert_eq!(text, "OK");
                                }
                                assert_eq!(requests.load(Ordering::SeqCst), sequence as usize, "BYOK must not retry on its own");
                                break 'invocation;
                            }
                            _ => {}
                        }
                    }
                }
            }
            append(&app, &id, 3, C::FinishRun(Default::default())).await;
            drop(stream);
            server.abort();
        }
    }
}
