#[path = "support/fixtures.rs"]
mod fixtures;
#[path = "support/fake_provider.rs"]
mod fake_provider;

use axum::{
    body::{to_bytes, Body},
    http::{HeaderMap, Request},
    routing::any,
    Router,
};
use base64::{engine::general_purpose::STANDARD, Engine};
use cursor_server::model::{
    model_hash, normalize_model_input, resolve_request_url, ModelConfigInput, ModelType,
};
use cursor_server::{
    api::cursor,
    config::{ProviderConfig, ProviderKind},
    cursor::{
        prompting::{PromptAssets, PromptCompiler},
        protocol::{connect, proto::{agent::v1 as agent_pb, aiserver::v1 as ai, inference as pb}},
        transport::TransportRegistry,
        TransportCommand,
    },
    network::NetworkClients,
    provider::{
        build_provider,
        image_generation::{generate, IMAGE_TIMEOUT},
        FinishReason, ModelEvent,
    },
    store::{ImageGenerationSettings, Store},
};
use prost::Message;
use serde_json::json;
use std::{
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio_util::sync::CancellationToken;
use tower::ServiceExt;

const RPC: &str = "/aiserver.v1.AiService/RunGenerateImage";
type Received = Arc<Mutex<Vec<(String, HeaderMap, Vec<u8>)>>>;

fn control(store: Store) -> (Router, fake_provider::FakeProvider) {
    let provider = fake_provider::FakeProvider::default();
    let service = cursor_server::control::ControlService::new(
        store.clone(), Arc::new(provider.clone()), NetworkClients::new(store), "image-test".into(),
    ).unwrap();
    (cursor_server::control::api_router(service), provider)
}

fn test_request(hash: &str, id: &str, body: Option<serde_json::Value>) -> Request<Body> {
    let request = Request::post(format!("/__byok-api__/api/models/{hash}/test/{id}"));
    match body {
        Some(body) => request.header("content-type", "application/json").body(Body::from(body.to_string())).unwrap(),
        None => request.body(Body::empty()).unwrap(),
    }
}

#[tokio::test]
async fn control_image_tests_require_description_and_forward_it_without_changing_default() {
    for endpoint in ["/v1/images/generations", "responses-image-generation"] {
        let data = png();
        let value = if endpoint == "/v1/images/generations" {
            json!({"data":[{"b64_json":data}]})
        } else {
            json!({"status":"completed","output":[{"type":"image_generation_call","status":"completed","result":data}]})
        };
        let (url, received, server) = mock(200, value, Duration::ZERO).await;
        let (_dir, store) = fixtures::temp_store().await;
        let model = store.create_model(&input(endpoint, &url)).await.unwrap();
        let (app, _) = control(store.clone());
        for body in [None, Some(json!({})), Some(json!({"image_prompt":" \n\t"}))] {
            let response = app.clone().oneshot(test_request(&model.model_hash, "missing", body)).await.unwrap();
            assert_eq!(response.status(), 400, "missing image prompt must fail before invoking provider");
        }
        assert!(received.lock().unwrap().is_empty());
        let prompt = "画一只坐在窗边的橘猫，水彩风格。";
        let response = app.oneshot(test_request(&model.model_hash, "custom", Some(json!({"image_prompt":prompt})))).await.unwrap();
        assert_eq!(response.status(), 200);
        let result: serde_json::Value = serde_json::from_slice(&to_bytes(response.into_body(), 1024 * 1024).await.unwrap()).unwrap();
        assert_eq!(result["image"]["width"], 2);
        assert_eq!(store.image_generation_settings().await.unwrap().model_hash, "");
        let requests = received.lock().unwrap();
        assert_eq!(requests.len(), 1);
        let body: serde_json::Value = serde_json::from_slice(&requests[0].2).unwrap();
        assert_eq!(body["model"], "provider-model");
        if endpoint == "/v1/images/generations" {
            assert_eq!(body["prompt"], prompt);
        } else {
            assert_eq!(body["input"][0]["content"][0]["text"], prompt);
        }
        server.abort();
    }
}

#[tokio::test]
async fn control_image_test_can_be_cancelled_while_waiting_for_provider() {
    for endpoint in ["/v1/images/generations", "responses-image-generation"] {
        let (url, received, server) = mock(200, json!({}), Duration::from_secs(60)).await;
        let (_dir, store) = fixtures::temp_store().await;
        let model = store.create_model(&input(endpoint, &url)).await.unwrap();
        let (app, _) = control(store);
        let pending = tokio::spawn(app.clone().oneshot(test_request(&model.model_hash, "cancel", Some(json!({"image_prompt":"draw a cat"})))));
        tokio::time::timeout(Duration::from_secs(3), async {
            while received.lock().unwrap().is_empty() { tokio::time::sleep(Duration::from_millis(5)).await; }
        }).await.unwrap();
        let cancelled = app.oneshot(Request::delete(format!("/__byok-api__/api/models/{}/test/cancel", model.model_hash)).body(Body::empty()).unwrap()).await.unwrap();
        assert_eq!(cancelled.status(), 204);
        let response = tokio::time::timeout(Duration::from_secs(1), pending).await.unwrap().unwrap().unwrap();
        assert_ne!(response.status(), 200);
        server.abort();
    }
}

#[tokio::test]
async fn control_chat_test_without_body_keeps_the_chat_test_prompt() {
    use cursor_server::provider::{ModelEvent, FinishReason};
    let (_dir, store) = fixtures::temp_store().await;
    let model = store.create_model(&input("/v1/responses", "http://localhost")).await.unwrap();
    let (app, provider) = control(store);
    provider.push(vec![ModelEvent::TextDelta("1 2 3".into()), ModelEvent::Done(FinishReason::Stop)]);
    let response = app.oneshot(test_request(&model.model_hash, "chat", None)).await.unwrap();
    assert_eq!(response.status(), 200);
    let request = provider.requests().pop().unwrap();
    assert!(format!("{:?}", request.history).contains("Output the numbers 1 through 120"));
}

fn png() -> String {
    let image = image::DynamicImage::new_rgb8(2, 3);
    let mut bytes = std::io::Cursor::new(Vec::new());
    image.write_to(&mut bytes, image::ImageFormat::Png).unwrap();
    STANDARD.encode(bytes.into_inner())
}

#[tokio::test]
async fn control_image_options_override_only_the_selected_test_and_use_protocol_specific_location() {
    for endpoint in ["/v1/images/generations", "responses-image-generation"] {
        let data = png();
        let value = if endpoint == "/v1/images/generations" {
            json!({"data":[{"b64_json":data}]})
        } else {
            json!({"status":"completed","output":[{"type":"image_generation_call","status":"completed","result":data}]})
        };
        let (url, received, server) = mock(200, value, Duration::ZERO).await;
        let (_dir, store) = fixtures::temp_store().await;
        let mut config = input(endpoint, &url);
        config.openai_extra_params_enabled = true;
        config.openai_extra_params = if endpoint == "/v1/images/generations" {
            json!({"quality":"low","size":"1024x1024","private_extension":"kept"})
        } else {
            json!({"private_extension":"kept","tools":[{"type":"image_generation","model":"separate-image-model","quality":"low","size":"1024x1024","tool_extension":"kept"}]})
        };
        let model = store.create_model(&config).await.unwrap();
        let (app, _) = control(store.clone());
        let options = json!({"size":"1536x864","quality":"high","output_format":"webp","background":"transparent","output_compression":75,"moderation":"auto","complexity":"medium"});
        let response = app.oneshot(test_request(&model.model_hash, "options", Some(json!({"image_prompt":"custom image","image_options":options})))).await.unwrap();
        assert_eq!(response.status(), 200);
        let saved = store.model(&model.model_hash).await.unwrap().unwrap();
        assert_eq!(saved.openai_extra_params, config.openai_extra_params);
        let requests = received.lock().unwrap();
        assert_eq!(requests.len(), 1);
        let body: serde_json::Value = serde_json::from_slice(&requests[0].2).unwrap();
        assert_eq!(body["private_extension"], "kept");
        let target = if endpoint == "/v1/images/generations" { &body } else { &body["tools"][0] };
        for (key, value) in options.as_object().unwrap() { assert_eq!(&target[key], value, "{endpoint}: {key}"); }
        if endpoint == "responses-image-generation" {
            assert!(body.get("background").is_none());
            assert!(body.get("size").is_none());
            assert_eq!(target["model"], "separate-image-model");
            assert_eq!(target["tool_extension"], "kept");
        }
        server.abort();
    }
}

#[tokio::test]
async fn invalid_image_test_options_do_not_invoke_the_provider() {
    for endpoint in ["/v1/images/generations", "responses-image-generation"] {
        let (url, received, server) = mock(200, json!({"data":[{"b64_json":png()}]}), Duration::ZERO).await;
        let (_dir, store) = fixtures::temp_store().await;
        let model = store.create_model(&input(endpoint, &url)).await.unwrap();
        let (app, _) = control(store);
        for options in [json!({"model":"override"}), json!({"n":2}), json!({"stream":true}),
            json!({"size":"invalid"}), json!({"size":"0x1024"}), json!({"quality":12}),
            json!({"output_format":"svg"}), json!({"output_compression":101}),
            json!({"output_compression":-1}), json!({"output_format":"jpeg","background":"transparent"}),
            json!({"output_format":"png","output_compression":50})] {
            let response = app.clone().oneshot(test_request(&model.model_hash, "invalid-options", Some(json!({"image_prompt":"draw","image_options":options})))).await.unwrap();
            assert_eq!(response.status(), 400, "invalid image option must fail before invoking provider");
        }
        assert!(received.lock().unwrap().is_empty());
        server.abort();
    }
}

async fn mock(
    status: u16,
    value: serde_json::Value,
    delay: Duration,
) -> (String, Received, tokio::task::JoinHandle<()>) {
    let received = Received::default();
    let capture = received.clone();
    let app = Router::new().fallback(any(move |request: Request<Body>| {
        let capture = capture.clone();
        let value = value.clone();
        async move {
            let (parts, body) = request.into_parts();
            let bytes = to_bytes(body, 40 * 1024 * 1024).await.unwrap();
            capture
                .lock()
                .unwrap()
                .push((parts.uri.to_string(), parts.headers, bytes.to_vec()));
            tokio::time::sleep(delay).await;
            (
                axum::http::StatusCode::from_u16(status).unwrap(),
                [("content-type", "application/json"), ("retry-after", "7")],
                value.to_string(),
            )
        }
    }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let handle = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (url, received, handle)
}

fn app(store: Store) -> Router {
    let provider = build_provider(&ProviderConfig {
        kind: ProviderKind::OpenAiChat,
        request_url: "http://127.0.0.1:1".into(),
        api_key: "unused".into(),
        custom_headers: Default::default(),
        max_output_tokens: None,
        request_timeout: Duration::from_secs(3),
        allowed_body_fields: None,
    })
    .unwrap();
    let assets =
        PromptAssets::load(&std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("prompt/cursor"))
            .unwrap();
    cursor::router(
        TransportRegistry::new(store.clone(), provider, PromptCompiler::new(assets)),
        NetworkClients::new(store),
    )
    .unwrap()
}

async fn native(app: Router, request: ai::RunGenerateImageRequest) -> ai::RunGenerateImageResponse {
    let response = app
        .oneshot(
            Request::post(RPC)
                .header("connect-timeout-ms", "2000")
                .body(Body::from(request.encode_to_vec()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    assert_eq!(response.headers()["content-type"], "application/proto");
    ai::RunGenerateImageResponse::decode(
        to_bytes(response.into_body(), 40 * 1024 * 1024)
            .await
            .unwrap(),
    )
    .unwrap()
}

#[tokio::test]
async fn saved_image_options_are_relocated_to_the_active_protocol() {
    let data = png();
    for (endpoint, extra, response) in [
        (
            "responses-image-generation",
            json!({"size":"1536x864", "quality":"high", "background":"opaque", "custom_root":{"keep":true}}),
            json!({"status":"completed", "output":[{"type":"image_generation_call", "status":"completed", "result":data.clone()}]}),
        ),
        (
            "/v1/images/generations",
            json!({"tools":[{"type":"image_generation", "size":"1536x864", "quality":"high", "custom_tool":1}], "custom_root":{"keep":true}}),
            json!({"data":[{"b64_json":data}]}),
        ),
    ] {
        let (url, received, server) = mock(200, response, Duration::ZERO).await;
        let (_dir, store) = fixtures::temp_store().await;
        let mut config = input(endpoint, &url);
        config.openai_extra_params_enabled = true;
        config.openai_extra_params = extra;
        let model = store.create_model(&config).await.unwrap();
        generate(
            &store,
            &model,
            &ai::RunGenerateImageRequest {
                description: "draw".into(),
                ..Default::default()
            },
            &CancellationToken::new(),
            IMAGE_TIMEOUT,
        )
        .await
        .unwrap();
        let requests = received.lock().unwrap();
        let body: serde_json::Value = serde_json::from_slice(&requests[0].2).unwrap();
        assert_eq!(body["custom_root"], json!({"keep":true}));
        if endpoint == "responses-image-generation" {
            assert!(body.get("size").is_none());
            assert!(body.get("quality").is_none());
            assert!(body.get("background").is_none());
            assert_eq!(body["tools"][0]["size"], "1536x864");
            assert_eq!(body["tools"][0]["quality"], "high");
            assert_eq!(body["tools"][0]["background"], "opaque");
        } else {
            assert!(body.get("tools").is_none());
            assert_eq!(body["size"], "1536x864");
            assert_eq!(body["quality"], "high");
        }
        server.abort();
    }
}

#[tokio::test]
async fn agent_prompt_exposes_generate_image_only_when_a_default_image_model_is_configured() {
    for configured in [false, true] {
        let (_dir, store) = fixtures::temp_store().await;
        let chat = store
            .create_model(&input("/v1/responses", "http://localhost"))
            .await
            .unwrap();
        if configured {
            let image = store
                .create_model(&input("/v1/images/generations", "http://localhost"))
                .await
                .unwrap();
            store
                .set_image_generation_settings(ImageGenerationSettings {
                    model_hash: image.model_hash,
                })
                .await
                .unwrap();
        }
        let provider = fake_provider::FakeProvider::default();
        provider.push(vec![
            ModelEvent::Start {
                model_call_id: "image-capability".into(),
            },
            ModelEvent::TextStart,
            ModelEvent::TextDelta("ok".into()),
            ModelEvent::TextEnd,
            ModelEvent::Done(FinishReason::Stop),
        ]);
        let assets = PromptAssets::load(
            &std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("prompt/cursor")
                .as_path(),
        )
        .unwrap();
        let registry = TransportRegistry::new(
            store,
            Arc::new(provider.clone()),
            PromptCompiler::new(assets),
        );
        let handle = registry
            .get_or_create(&format!("image-capability-{configured}"))
            .await
            .unwrap();
        let mut output = handle.subscribe();
        handle
            .command(TransportCommand::Append {
                seqno: 0,
                message: Box::new(agent_run(&chat.model_hash)),
            })
            .await
            .unwrap();
        let mut seqno = 1;
        let request = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if let Some(request) = provider.requests().into_iter().next() {
                    break Some(request);
                }
                let Some(frame) = output.recv().await else {
                    break None;
                };
                let (flags, payload) = connect::decode_frames(&frame)
                    .expect("server frame")
                    .pop()
                    .expect("server frame payload");
                if flags & connect::END_STREAM_FLAG != 0 {
                    break None;
                }
                let server = agent_pb::AgentServerMessage::decode(payload)
                    .expect("server message");
                if let Some(agent_pb::agent_server_message::Message::KvServerMessage(kv)) =
                    server.message
                {
                    handle
                        .command(TransportCommand::Append {
                            seqno,
                            message: Box::new(kv_ack(kv.id)),
                        })
                        .await
                        .unwrap();
                    seqno += 1;
                }
            }
        })
        .await;
        let request = match request {
            Ok(Some(request)) => request,
            _ => panic!(
                "agent run must reach the provider; configured={configured}; provider_requests={}",
                provider.requests().len()
            ),
        };
        let tools = request
            .prompt
            .tools
            .iter()
            .map(|tool| tool.name.as_str())
            .collect::<Vec<_>>();
        assert_eq!(
            tools.contains(&"GenerateImage"),
            configured,
            "configured={configured}, tools={tools:?}"
        );
        drop(output);
    }
}

fn agent_run(model_id: &str) -> agent_pb::AgentClientMessage {
    let user = agent_pb::UserMessage {
        text: "hello".into(),
        message_id: "image-capability-user".into(),
        mode: agent_pb::AgentMode::Agent as i32,
        ..Default::default()
    };
    agent_pb::AgentClientMessage {
        message: Some(agent_pb::agent_client_message::Message::RunRequest(
            agent_pb::AgentRunRequest {
                action: Some(agent_pb::ConversationAction {
                    action: Some(agent_pb::conversation_action::Action::UserMessageAction(
                        agent_pb::UserMessageAction {
                            user_message: Some(user),
                            request_context: Some(agent_pb::RequestContext::default()),
                            ..Default::default()
                        },
                    )),
                    ..Default::default()
                }),
                conversation_id: Some("image-capability-conversation".into()),
                run_id: Some("image-capability-run".into()),
                requested_model: Some(agent_pb::RequestedModel {
                    model_id: model_id.into(),
                    ..Default::default()
                }),
                ..Default::default()
            },
        )),
    }
}

fn kv_ack(id: u32) -> agent_pb::AgentClientMessage {
    agent_pb::AgentClientMessage {
        message: Some(agent_pb::agent_client_message::Message::KvClientMessage(
            agent_pb::KvClientMessage {
                id,
                message: Some(agent_pb::kv_client_message::Message::SetBlobResult(
                    agent_pb::SetBlobResult { error: None },
                )),
            },
        )),
    }
}

fn input(endpoint: &str, base_url: &str) -> ModelConfigInput {
    serde_json::from_value(json!({
        "display_name":"image-test", "type":"openai", "base_url":base_url,
        "api_key":"image-test-secret", "tooltip_data":"test", "model_id":"provider-model",
        "openai_endpoint":endpoint
    }))
    .unwrap()
}

#[test]
fn image_protocol_urls_and_identity_are_distinct_from_chat() {
    for (endpoint, suffix) in [
        ("/v1/images/generations", "/v1/images/generations"),
        ("responses-image-generation", "/v1/responses"),
    ] {
        let model = input(endpoint, "http://localhost/api/v1");
        normalize_model_input(&model).unwrap();
        assert_eq!(
            resolve_request_url(ModelType::OpenAi, &model.base_url, endpoint, false).unwrap(),
            format!("http://localhost/api{suffix}")
        );
        let mut full = model.clone();
        full.use_full_url = true;
        full.base_url = "http://localhost/custom?version=1".into();
        assert_eq!(
            resolve_request_url(full.model_type, &full.base_url, endpoint, true).unwrap(),
            full.base_url
        );
        let mut chat = full.clone();
        chat.openai_endpoint = "/v1/responses".into();
        assert_ne!(model_hash(&full).unwrap(), model_hash(&chat).unwrap());
    }
}

#[tokio::test]
async fn native_image_protocols_use_default_model_references_and_return_validated_bytes() {
    for endpoint in ["/v1/images/generations", "responses-image-generation"] {
        for references in [false, true] {
            let data = png();
            let response = if endpoint == "/v1/images/generations" {
                json!({"data":[{"b64_json":data}]})
            } else {
                json!({"status":"completed", "output":[{"type":"message", "content":[]},{"type":"image_generation_call","status":"completed","result":data}]})
            };
            let (url, received, server) = mock(200, response, Duration::ZERO).await;
            let (_dir, store) = fixtures::temp_store().await;
            let mut config = input(endpoint, &format!("{url}/prefix/v1"));
            config.reasoning_effort = Some("high".into());
            config.context_window_tokens = Some(1_000_000);
            config.max_completion_tokens = Some(16384);
            config.custom_headers_enabled = true;
            config.custom_headers = json!({"x-test-header":"private-test-header"});
            config.openai_extra_params_enabled = true;
            config.openai_extra_params = if endpoint == "/v1/images/generations" {
                json!({"quality":"low", "output_format":"png", "custom_field":"preserved"})
            } else {
                json!({"tools":[{"type":"image_generation", "model":"separate-image-model", "quality":"low"}], "custom_field":"preserved"})
            };
            let model = store.create_model(&config).await.unwrap();
            assert!(model.is_image_generation());
            assert_eq!(model.reasoning_effort, None);
            assert_eq!(model.context_window_tokens, None);
            assert_eq!(model.max_completion_tokens, None);
            store
                .set_image_generation_settings(ImageGenerationSettings {
                    model_hash: model.model_hash.clone(),
                })
                .await
                .unwrap();
            let result = native(
                app(store),
                ai::RunGenerateImageRequest {
                    description: "test drawing".into(),
                    model_id: "parent-chat-model-must-not-be-used".into(),
                    max_mode: true,
                    aspect_ratio: Some("3:2".into()),
                    reference_images: if references {
                        vec![ai::GenerateImageReferenceImage {
                            data: data.clone(),
                            mime_type: "image/png".into(),
                        }]
                    } else {
                        vec![]
                    },
                },
            )
            .await;
            let Some(ai::run_generate_image_response::Result::Success(image)) = result.result
            else {
                panic!("native image generation did not succeed")
            };
            assert_eq!(image.mime_type, "image/png");
            assert_eq!(image.image_data, data);
            let requests = received.lock().unwrap();
            assert_eq!(requests.len(), 1);
            let (path, headers, bytes) = &requests[0];
            assert_eq!(headers["authorization"], "Bearer image-test-secret");
            assert_eq!(headers["x-test-header"], "private-test-header");
            if endpoint == "/v1/images/generations" && references {
                assert_eq!(path, "/prefix/v1/images/edits");
                assert!(headers["content-type"]
                    .to_str()
                    .unwrap()
                    .starts_with("multipart/form-data; boundary="));
                let body = String::from_utf8_lossy(bytes);
                for field in [
                    "name=\"image[]\"",
                    "Content-Type: image/png",
                    "provider-model",
                    "1536x1024",
                    "preserved",
                ] {
                    assert!(body.contains(field), "missing multipart field {field}");
                }
            } else {
                let body: serde_json::Value = serde_json::from_slice(bytes).unwrap();
                assert_eq!(body["model"], "provider-model");
                assert_eq!(body["custom_field"], "preserved");
                assert!(body.get("reasoning").is_none());
                assert!(body.get("max_output_tokens").is_none());
                if endpoint == "/v1/images/generations" {
                    assert_eq!(path, "/prefix/v1/images/generations");
                    assert_eq!(body["size"], "1536x1024");
                    assert_eq!(body["n"], 1);
                } else {
                    assert_eq!(path, "/prefix/v1/responses");
                    assert_eq!(body["tools"][0]["model"], "separate-image-model");
                    assert_eq!(body["tools"][0]["size"], "1536x1024");
                    assert_eq!(body["tool_choice"]["type"], "image_generation");
                    assert_eq!(body["stream"], false);
                    if references {
                        assert_eq!(
                            body["input"][0]["content"][1]["image_url"],
                            format!("data:image/png;base64,{data}")
                        );
                    }
                }
            }
            server.abort();
        }
    }
}

#[tokio::test]
async fn native_image_errors_preserve_status_safety_and_retry_without_fallback_or_secrets() {
    for endpoint in ["/v1/images/generations", "responses-image-generation"] {
        for status in [400, 401, 429, 503] {
            let (url, received, server) = mock(status, json!({"error":{"code":"content_policy_violation", "message":"rejected image-test-secret PRIVATE_PROMPT"}, "input":"PRIVATE_BODY"}), Duration::ZERO).await;
            let (_dir, store) = fixtures::temp_store().await;
            let model = store.create_model(&input(endpoint, &url)).await.unwrap();
            store
                .set_image_generation_settings(ImageGenerationSettings {
                    model_hash: model.model_hash,
                })
                .await
                .unwrap();
            let result = native(
                app(store),
                ai::RunGenerateImageRequest {
                    description: "PRIVATE_PROMPT".into(),
                    ..Default::default()
                },
            )
            .await;
            let Some(ai::run_generate_image_response::Result::Error(error)) = result.result else {
                panic!("expected native error")
            };
            assert_eq!(error.provider_status_code, Some(i32::from(status)));
            assert!(error.content_safety_blocked);
            assert!(error.error.contains("Retry-After: 7"));
            for secret in ["image-test-secret", "PRIVATE_PROMPT", "PRIVATE_BODY"] {
                assert!(!error.error.contains(secret));
            }
            assert_eq!(received.lock().unwrap().len(), 1);
            server.abort();
        }
    }
}

#[tokio::test]
async fn empty_corrupt_incomplete_and_text_only_successes_are_rejected() {
    for (endpoint, value) in [
        ("/v1/images/generations", json!({"data":[]})),
        (
            "/v1/images/generations",
            json!({"data":[{"b64_json":"not-base64"}]}),
        ),
        (
            "/v1/images/generations",
            json!({"data":[{"b64_json":STANDARD.encode("not an image") }]}),
        ),
        (
            "responses-image-generation",
            json!({"status":"incomplete", "output":[{"type":"image_generation_call", "status":"completed", "result":png()}]}),
        ),
        (
            "responses-image-generation",
            json!({"status":"completed", "output":[{"type":"message", "content":[{"type":"output_text", "text":"made it"}]}]}),
        ),
    ] {
        let (url, received, server) = mock(200, value, Duration::ZERO).await;
        let (_dir, store) = fixtures::temp_store().await;
        let model = store.create_model(&input(endpoint, &url)).await.unwrap();
        let result = generate(
            &store,
            &model,
            &ai::RunGenerateImageRequest {
                description: "draw".into(),
                ..Default::default()
            },
            &CancellationToken::new(),
            IMAGE_TIMEOUT,
        )
        .await;
        assert!(result.is_err());
        assert_eq!(received.lock().unwrap().len(), 1);
        server.abort();
    }
}

#[tokio::test]
async fn image_settings_reject_chat_keep_selection_on_edit_and_require_explicit_disable() {
    let (_dir, store) = fixtures::temp_store().await;
    let chat = store
        .create_model(&input("/v1/responses", "http://localhost"))
        .await
        .unwrap();
    assert!(store
        .set_image_generation_settings(ImageGenerationSettings {
            model_hash: chat.model_hash.clone()
        })
        .await
        .is_err());
    let mut config = input("responses-image-generation", "http://localhost");
    let image = store.create_model(&config).await.unwrap();
    assert_eq!(store.chat_models().await.unwrap().len(), 1);
    assert_eq!(store.models().await.unwrap().len(), 2);
    let metadata = cursor_server::cursor::inference::prompt_metadata(
        &pb::RunInferenceRunRequest {
            requested_model: Some(pb::InferenceRequestedModel {
                model_id: chat.model_hash.clone(),
                ..Default::default()
            }),
            ..Default::default()
        },
        Some(&chat),
        &store.models().await.unwrap(),
    );
    assert_eq!(metadata.subagent_model_config.unwrap().models.len(), 1);
    store
        .set_image_generation_settings(ImageGenerationSettings {
            model_hash: image.model_hash.clone(),
        })
        .await
        .unwrap();
    assert!(store.delete_model(&image.model_hash).await.is_err());
    config.display_name = "renamed image".into();
    let updated = store
        .update_model(&image.model_hash, &config)
        .await
        .unwrap();
    assert_ne!(image.model_hash, updated.model_hash);
    assert_eq!(
        store.image_generation_settings().await.unwrap().model_hash,
        updated.model_hash
    );
    config.openai_endpoint = "/v1/responses".into();
    assert!(store
        .update_model(&updated.model_hash, &config)
        .await
        .is_err());
    store
        .set_image_generation_settings(ImageGenerationSettings::default())
        .await
        .unwrap();
    store.delete_model(&updated.model_hash).await.unwrap();
}

#[tokio::test]
async fn image_cancellation_and_deadline_do_not_wait_for_the_provider() {
    for endpoint in ["/v1/images/generations", "responses-image-generation"] {
        let (url, _, server) = mock(200, json!({}), Duration::from_secs(60)).await;
        let (_dir, store) = fixtures::temp_store().await;
        let model = store.create_model(&input(endpoint, &url)).await.unwrap();
        let request = ai::RunGenerateImageRequest {
            description: "draw".into(),
            ..Default::default()
        };
        let cancel = CancellationToken::new();
        let future = generate(&store, &model, &request, &cancel, IMAGE_TIMEOUT);
        let result = tokio::join!(future, async {
            tokio::time::sleep(Duration::from_millis(30)).await;
            cancel.cancel();
        })
        .0;
        assert!(result.unwrap_err().to_string().contains("cancelled"));
        let error = generate(
            &store,
            &model,
            &request,
            &CancellationToken::new(),
            Duration::from_millis(20),
        )
        .await
        .unwrap_err();
        assert!(error.to_string().contains("timed out"));
        server.abort();
    }
}

#[tokio::test]
async fn image_downloads_never_receive_provider_credentials_or_custom_headers() {
    use axum::http::StatusCode;
    let data = STANDARD.decode(png()).unwrap();
    let image_server = Router::new().route(
        "/image",
        axum::routing::get(move |headers: HeaderMap| {
            let data = data.clone();
            async move {
                assert!(!headers.contains_key("authorization"));
                assert!(!headers.contains_key("x-private-header"));
                (StatusCode::OK, [("content-type", "image/png")], data)
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let image_url = format!("http://{}/image", listener.local_addr().unwrap());
    let image_task =
        tokio::spawn(async move { axum::serve(listener, image_server).await.unwrap() });
    let (url, _, server) = mock(200, json!({"data":[{"url":image_url}]}), Duration::ZERO).await;
    let (_dir, store) = fixtures::temp_store().await;
    let mut config = input("/v1/images/generations", &url);
    config.custom_headers_enabled = true;
    config.custom_headers = json!({"x-private-header":"never-forward"});
    let model = store.create_model(&config).await.unwrap();
    let image = generate(
        &store,
        &model,
        &ai::RunGenerateImageRequest {
            description: "draw".into(),
            ..Default::default()
        },
        &CancellationToken::new(),
        IMAGE_TIMEOUT,
    )
    .await
    .unwrap();
    assert_eq!((image.width, image.height), (2, 3));
    server.abort();
    image_task.abort();
}

#[tokio::test]
async fn invalid_reference_mime_protected_params_and_custom_edits_routes_fail_before_sending() {
    let (url, received, server) = mock(200, json!({}), Duration::ZERO).await;
    let (_dir, store) = fixtures::temp_store().await;
    let mut config = input("/v1/images/generations", &url);
    let reference = ai::GenerateImageReferenceImage {
        data: png(),
        mime_type: "image/png".into(),
    };
    for (full_url, extra, mime) in [
        (false, json!({"model":"override"}), "image/png"),
        (false, json!({}), "image/jpeg"),
        (true, json!({}), "image/png"),
    ] {
        config.use_full_url = full_url;
        config.openai_extra_params_enabled = true;
        config.openai_extra_params = extra;
        let model = store.create_model(&config).await.unwrap();
        let mut reference = reference.clone();
        reference.mime_type = mime.into();
        let request = ai::RunGenerateImageRequest {
            description: "draw".into(),
            reference_images: vec![reference],
            ..Default::default()
        };
        assert!(generate(
            &store,
            &model,
            &request,
            &CancellationToken::new(),
            IMAGE_TIMEOUT
        )
        .await
        .is_err());
        store.delete_model(&model.model_hash).await.unwrap();
    }
    assert!(received.lock().unwrap().is_empty());
    server.abort();
}

#[tokio::test]
async fn dropping_native_rpc_body_closes_outbound_request() {
    use futures_util::StreamExt;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
    let upstream = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut bytes = Vec::new();
        let mut buf = [0; 4096];
        loop {
            let n = socket.read(&mut buf).await.unwrap();
            assert!(n > 0);
            bytes.extend_from_slice(&buf[..n]);
            if let Some(header_end) = bytes.windows(4).position(|p| p == b"\r\n\r\n") {
                let headers = String::from_utf8_lossy(&bytes[..header_end]).to_ascii_lowercase();
                let size = headers
                    .lines()
                    .find_map(|l| l.strip_prefix("content-length:"))
                    .unwrap()
                    .trim()
                    .parse::<usize>()
                    .unwrap();
                if bytes.len() >= header_end + 4 + size {
                    break;
                }
            }
        }
        socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 99999\r\n\r\n").await.unwrap();
        ready_tx.send(()).unwrap();
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(3), socket.read(&mut buf))
                .await
                .unwrap()
                .unwrap(),
            0
        );
    });
    let (_dir, store) = fixtures::temp_store().await;
    let model = store
        .create_model(&input("/v1/images/generations", &url))
        .await
        .unwrap();
    store
        .set_image_generation_settings(ImageGenerationSettings {
            model_hash: model.model_hash,
        })
        .await
        .unwrap();
    let response = app(store)
        .oneshot(
            Request::post(RPC)
                .body(Body::from(
                    ai::RunGenerateImageRequest {
                        description: "draw".into(),
                        ..Default::default()
                    }
                    .encode_to_vec(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let mut body = response.into_body().into_data_stream();
    let (_, ready) = tokio::join!(
        tokio::time::timeout(Duration::from_millis(100), body.next()),
        ready_rx
    );
    ready.unwrap();
    drop(body);
    upstream.await.unwrap();
}
