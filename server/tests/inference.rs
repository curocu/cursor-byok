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
    model::{
        ModelConfigInput, ModelType, ProjectedContent, Usage, OPENAI_CHAT_ENDPOINT,
        OPENAI_RESPONSES_ENDPOINT,
    },
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
    post_with_headers(app, path, message, &[]).await
}

#[tokio::test]
async fn local_loop_context_limit_matches_selected_window_for_all_provider_protocols() {
    use pb::run_inference_client_message::Message as C;
    use pb::run_inference_server_message::Message as S;

    let (_dir, store) = fixtures::temp_store().await;
    let assets =
        PromptAssets::load(&std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("prompt/cursor"))
            .unwrap();
    let provider = Arc::new(fake_provider::FakeProvider::default());
    let registry =
        TransportRegistry::new(store.clone(), provider.clone(), PromptCompiler::new(assets));
    let app = cursor::router(registry, NetworkClients::new(store.clone())).unwrap();
    let cases = [
        (Some("1m"), None, 1_000_000),
        (Some("200k"), None, 200_000),
        (Some("1m"), Some(128_000), 1_000_000),
        (Some("200k"), Some(1_000_000), 200_000),
        (Some("128000"), None, 128_000),
        (None, Some(128_000), 128_000),
        (None, None, 200_000),
    ];
    for (provider_index, (model_type, endpoint)) in [
        (ModelType::OpenAi, OPENAI_CHAT_ENDPOINT),
        (ModelType::OpenAi, OPENAI_RESPONSES_ENDPOINT),
        (ModelType::Anthropic, OPENAI_CHAT_ENDPOINT),
    ]
    .into_iter()
    .enumerate()
    {
        for (index, (selected, configured, expected)) in cases.into_iter().enumerate() {
            let id = format!("context-limit-{provider_index}-{index}");
            let mut input = model_input();
            input.model_id = id.clone();
            input.model_type = model_type;
            input.openai_endpoint = endpoint.into();
            input.context_window_tokens = configured;
            let model = store.create_model(&input).await.unwrap();
            append(
                &app,
                &id,
                0,
                C::RunRequest(pb::RunInferenceRunRequest {
                    conversation_id: id.clone(),
                    requested_model: Some(pb::InferenceRequestedModel {
                        model_id: model.model_hash.clone(),
                        parameters: selected
                            .map(|value| pb::InferenceModelParameterValue {
                                id: "context".into(),
                                value: value.into(),
                            })
                            .into_iter()
                            .collect(),
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
                    request_id: id.clone(),
                },
            )
            .await;
            assert_eq!(response.status(), 200);
            let mut stream = response.into_body().into_data_stream();
            let S::RunReady(ready) = next(&mut stream).await else {
                panic!("expected run_ready for {id}");
            };
            // Cursor harness 431 replaces its complete managed-local defaults
            // when the server supplies any feature flags. Keep those behaviors
            // enabled for every provider; adding Steer must not disable them.
            let flags = &ready.prompt_model_metadata.as_ref().unwrap().feature_flags;
            for key in [
                "closeStdin",
                "enableAwaitForSubagents",
                "enableBackgroundTaskProgress",
                "enableBoundedSubagentAwait",
                "enableDebugSubagent",
                "enableEmptyResponseRetry",
                "enableExploreSubagent",
                "enableReadToolNegativeOffset",
                "enableSandboxSharedBuildCache",
                "longRunningJobs",
                "nalLoopDetection",
                "subagentSupportInterrupt",
                "useClientSideSubagent",
            ] {
                assert_eq!(
                    flags.get(key).and_then(|flag| flag.value.as_ref()),
                    Some(&pb::run_inference_flag_value::Value::BoolValue(true)),
                    "{id}: native managed-local default {key} was disabled"
                );
            }
            assert_eq!(
                flags
                    .get("disableBackgroundTaskFollowUp")
                    .and_then(|f| f.value.as_ref()),
                Some(&pb::run_inference_flag_value::Value::BoolValue(false)),
                "{id}: native background follow-up must remain enabled"
            );
            assert_eq!(
                flags
                    .get("outputNotificationLimit")
                    .and_then(|f| f.value.as_ref()),
                Some(&pb::run_inference_flag_value::Value::NumberValue(1000.0)),
                "{id}: keep Cursor's native notification bound"
            );
            for key in [
                "steerPreemptIdleSampleGraceMs",
                "steerPreemptToolCallWriteGraceMs",
            ] {
                assert_eq!(
                    ready
                        .prompt_model_metadata
                        .as_ref()
                        .unwrap()
                        .feature_flags
                        .get(key)
                        .and_then(|flag| flag.value.as_ref()),
                    Some(&pb::run_inference_flag_value::Value::NumberValue(1000.0)),
                    "{id}: native steer preemption policy {key} is missing"
                );
            }
            assert!(
                ready.supports_self_summary,
                "{id}: Cursor self-summary is required"
            );
            assert!(
                ready.summarization.is_none(),
                "{id}: keep Cursor's native policy"
            );
            assert_eq!(
                ready.prompt_model_metadata.unwrap().agent_token_limit,
                Some(expected),
                "{id}: selected={selected:?}, configured={configured:?}"
            );
            for round in 1..=3 {
                let (purpose, input_text, output_text, input_tokens) = match round {
                    1 => (0, "Original conversation", "OK", 120),
                    2 => (
                        1,
                        "Summarize the original conversation",
                        "Conversation summary",
                        120,
                    ),
                    _ => (
                        0,
                        "Conversation summary\nContinue the task",
                        "Continued",
                        30,
                    ),
                };
                provider.push(vec![
                    ModelEvent::TextDelta(output_text.into()),
                    ModelEvent::Usage(Usage {
                        input_tokens: Some(input_tokens),
                        output_tokens: Some(10),
                        total_tokens: Some(input_tokens + 10),
                        ..Default::default()
                    }),
                    ModelEvent::Done(FinishReason::Stop),
                ]);
                append(
                    &app,
                    &id,
                    round,
                    C::InvokeModel(pb::RunInferenceInvokeModel {
                        invocation_id: format!("{id}-round-{round}"),
                        purpose,
                        request: Some(pb::InferenceStreamRequest {
                            messages: vec![pb::InferenceCoreMessage {
                                role: 1,
                                content: Some(pb::inference_core_message::Content::Text(
                                    input_text.into(),
                                )),
                                ..Default::default()
                            }],
                            ..Default::default()
                        }),
                        ..Default::default()
                    }),
                )
                .await;
                let mut usage = None;
                let mut supports_self_summary = None;
                let mut response_text = None;
                loop {
                    match next(&mut stream).await {
                        S::InvocationResponse(response) => {
                            match response.response.unwrap().response {
                                Some(pb::inference_stream_response::Response::ExtendedUsage(v)) => {
                                    usage = Some((v.max_tokens, v.input_tokens, v.output_tokens));
                                }
                                Some(pb::inference_stream_response::Response::ResponseInfo(v)) => {
                                    supports_self_summary = v.supports_self_summary;
                                    assert!(v.early_compaction_context_token_threshold.is_none());
                                    assert!(v.self_summary_flavor.is_none());
                                    response_text =
                                        v.messages.first().and_then(|m| m.content.clone());
                                }
                                _ => {}
                            }
                        }
                        S::InvocationEnd(end) => {
                            assert!(end.error.is_none(), "{id}: {:?}", end.error);
                            break;
                        }
                        _ => {}
                    }
                }
                assert_eq!(
                    usage,
                    Some((expected, input_tokens as i32, 10)),
                    "{id}, round {round}"
                );
                assert_eq!(supports_self_summary, Some(true), "{id}, round {round}");
                assert_eq!(
                    response_text.as_deref(),
                    Some(output_text),
                    "{id}, round {round}"
                );
                let requests = provider.requests();
                let request = requests.last().unwrap();
                assert_eq!(request.model.model_id, model.model_hash);
                assert_eq!(request.history.len(), 1);
                assert!(
                    matches!(&request.history[0].content,
                    ProjectedContent::Parts(parts) if matches!(&parts[..],
                        [cursor_server::model::ContentPart::Text { text }] if text == input_text)),
                    "{id}, round {round}: preserve Cursor's supplied history"
                );
            }
        }
    }
}

async fn post_with_headers(
    app: &Router,
    path: &str,
    message: &impl Message,
    headers: &[(&str, &str)],
) -> axum::response::Response {
    let mut request = Request::post(path).header("content-type", "application/proto");
    for (name, value) in headers {
        request = request.header(*name, *value);
    }
    app.clone()
        .oneshot(request.body(Body::from(message.encode_to_vec())).unwrap())
        .await
        .unwrap()
}

async fn append(
    app: &Router,
    id: &str,
    seq: i64,
    message: pb::run_inference_client_message::Message,
) {
    append_with_headers(app, id, seq, message, &[]).await
}

async fn append_with_headers(
    app: &Router,
    id: &str,
    seq: i64,
    message: pb::run_inference_client_message::Message,
    headers: &[(&str, &str)],
) {
    let message = pb::RunInferenceClientMessage {
        message: Some(message),
    };
    let response = post_with_headers(
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
        headers,
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
async fn output_limit_closes_only_the_invocation_and_keeps_native_run_usable() {
    use pb::inference_stream_response::Response as R;
    use pb::run_inference_client_message::Message as C;
    use pb::run_inference_server_message::Message as S;
    let (_dir, store) = fixtures::temp_store().await;
    let model = store.create_model(&model_input()).await.unwrap();
    let provider = Arc::new(fake_provider::FakeProvider::default());
    for reason in [
        FinishReason::Length,
        FinishReason::Length,
        FinishReason::Stop,
    ] {
        provider.push(vec![
            ModelEvent::TextDelta("partial answer".into()),
            ModelEvent::Usage(Usage {
                input_tokens: Some(120),
                output_tokens: Some(10),
                total_tokens: Some(130),
                ..Default::default()
            }),
            ModelEvent::Done(reason),
        ]);
    }
    let assets =
        PromptAssets::load(&std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("prompt/cursor"))
            .unwrap();
    let registry =
        TransportRegistry::new(store.clone(), provider.clone(), PromptCompiler::new(assets));
    let app = cursor::router(registry, NetworkClients::new(store)).unwrap();
    let id = "output-limit-run";
    append(
        &app,
        id,
        0,
        C::RunRequest(pb::RunInferenceRunRequest {
            conversation_id: "output-limit-conversation".into(),
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
        panic!("expected run ready");
    };
    assert!(ready.supports_self_summary);
    for (index, purpose) in [0, 1, 0].into_iter().enumerate() {
        let invocation_id = format!("output-limit-{index}");
        append(
            &app,
            id,
            index as i64 + 1,
            C::InvokeModel(pb::RunInferenceInvokeModel {
                invocation_id: invocation_id.clone(),
                purpose,
                request: Some(pb::InferenceStreamRequest {
                    messages: vec![pb::InferenceCoreMessage {
                        role: 1,
                        content: Some(pb::inference_core_message::Content::Text("continue".into())),
                        ..Default::default()
                    }],
                    ..Default::default()
                }),
                ..Default::default()
            }),
        )
        .await;
        let mut ending = Vec::new();
        let mut text = String::new();
        let mut usage = None;
        let mut limit_message = None;
        loop {
            match next(&mut stream).await {
                S::InvocationResponse(response) => {
                    assert_eq!(response.invocation_id, invocation_id);
                    match response.response.unwrap().response.unwrap() {
                        R::Error(error) => {
                            assert!(index < 2);
                            assert!(error.is_output_token_limit_error);
                            assert!(!error.is_input_token_limit_error);
                            assert_eq!(error.error_type, pb::InferenceStreamErrorType::InferencestreamerrortypeOutputTokenLimit as i32);
                            limit_message = Some(error.message);
                            ending.push("error");
                        }
                        R::ResponseInfo(info) => {
                            assert_eq!(info.messages[0].content.as_deref(), Some("partial answer"));
                            assert_eq!(info.error_message, limit_message);
                            ending.push("info");
                        }
                        R::TextPart(part) if part.is_final => {
                            assert!(part.text.is_empty());
                            ending.push("final");
                        }
                        R::TextPart(part) => text.push_str(&part.text),
                        R::Usage(value) => usage = Some(value),
                        _ => {}
                    }
                }
                S::InvocationEnd(end) => {
                    assert_eq!(end.invocation_id, invocation_id);
                    assert!(
                        end.error.is_none(),
                        "typed model errors must not become transport errors"
                    );
                    break;
                }
                other => panic!("unexpected response: {other:?}"),
            }
        }
        assert_eq!(text, "partial answer");
        assert_eq!(usage.unwrap().total_tokens, Some(130));
        assert_eq!(
            ending,
            if index < 2 {
                vec!["error", "info", "final"]
            } else {
                vec!["info", "final"]
            }
        );
        assert_eq!(
            provider.requests().len(),
            index + 1,
            "BYOK must not add its own recovery calls"
        );
    }
    append(
        &app,
        id,
        4,
        C::FinishRun(pb::RunInferenceFinishRun::default()),
    )
    .await;
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
    assert!(ready.routed_model_display_name.is_none());
    let metadata = ready.prompt_model_metadata.unwrap();
    assert_eq!(
        metadata.feature_flags["dynamicToolProfile"].value,
        Some(pb::run_inference_flag_value::Value::StringValue(
            "all-static".into()
        ))
    );
    // Any explicit flag map replaces the harness defaults. Without this switch,
    // Task tries to build an unsupported in-process child AgentConfig.
    for flag in ["useClientSideSubagent", "longRunningJobs"] {
        assert_eq!(
            metadata
                .feature_flags
                .get(flag)
                .and_then(|f| f.value.as_ref()),
            Some(&pb::run_inference_flag_value::Value::BoolValue(true)),
            "local Task execution and background waiting require {flag}"
        );
    }
    for mode in ["agent", "chat", "plan", "debug", "triage", "multitask"] {
        let config = metadata
            .agent_mode_configs
            .iter()
            .find(|m| m.mode_id == mode)
            .expect("every Cursor mode must have a config");
        let targets = config.switch_mode_tool_config.as_ref().map(|switch| {
            switch
                .target_modes
                .iter()
                .map(String::as_str)
                .collect::<Vec<_>>()
        });
        let expected = match mode {
            "agent" | "multitask" => Some(vec!["plan"]),
            "plan" => Some(vec!["agent"]),
            _ => None,
        };
        assert_eq!(targets, expected, "mode switching targets for {mode}");
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

fn assert_user_abort(message: pb::run_inference_server_message::Message) {
    let pb::run_inference_server_message::Message::InvocationEnd(end) = message else {
        panic!("expected invocation end");
    };
    let error = end.error.expect("stopped invocation must carry an abort");
    assert_eq!(error.code, 10, "Cursor must not retry a user abort");
    assert_eq!(error.message, "User aborted request");
    assert_eq!(error.details.len(), 1);
    assert_eq!(error.details[0].r#type, "aiserver.v1.ErrorDetails");
    let details = ai::ErrorDetails::decode(error.details[0].value.as_slice()).unwrap();
    assert_eq!(details.error, 21, "Cursor's USER_ABORTED_REQUEST marker");
    assert_eq!(details.is_expected, Some(true));
}

#[derive(Clone, Copy, PartialEq)]
enum LineageSource {
    Invocation,
    AppendHeaders,
    SseHeaders,
    RetryHeaders,
}

async fn assert_parent_stop_cancels_descendants(
    stop_between_calls: bool,
    lineage_source: LineageSource,
) {
    use pb::run_inference_client_message::Message as C;
    use pb::run_inference_server_message::Message as S;
    let (_dir, store) = fixtures::temp_store().await;
    let model = store.create_model(&model_input()).await.unwrap();
    let provider = Arc::new(fake_provider::FakeProvider::default());
    let assets =
        PromptAssets::load(&std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("prompt/cursor"))
            .unwrap();
    let registry =
        TransportRegistry::new(store.clone(), provider.clone(), PromptCompiler::new(assets));
    let app = cursor::router(registry, NetworkClients::new(store)).unwrap();
    let mut streams = Vec::new();
    let parent_id = if lineage_source == LineageSource::RetryHeaders {
        "original-parent"
    } else {
        "parent"
    };
    for (id, parent, root) in [
        ("parent", None, None),
        ("child", Some(parent_id), Some(parent_id)),
        ("grandchild", Some("child"), Some(parent_id)),
        ("unrelated", None, None),
    ] {
        let mut headers: Vec<_> = parent
            .map(|parent| ("x-parent-request-id", parent))
            .into_iter()
            .chain(root.map(|root| ("x-root-parent-request-id", root)))
            .collect();
        if id == "parent" && lineage_source == LineageSource::RetryHeaders {
            headers.push(("x-original-request-id", parent_id));
        }
        append_with_headers(
            &app,
            id,
            0,
            C::RunRequest(pb::RunInferenceRunRequest {
                conversation_id: format!("conversation-{id}"),
                requested_model: Some(pb::InferenceRequestedModel {
                    model_id: model.model_hash.clone(),
                    ..Default::default()
                }),
                ..Default::default()
            }),
            if matches!(
                lineage_source,
                LineageSource::AppendHeaders | LineageSource::RetryHeaders
            ) {
                &headers
            } else {
                &[]
            },
        )
        .await;
        let response = post_with_headers(
            &app,
            "/aiserver.v1.InferenceService/RunInferenceSSE",
            &agent::BidiRequestId {
                request_id: id.into(),
            },
            if lineage_source == LineageSource::SseHeaders {
                &headers
            } else {
                &[]
            },
        )
        .await;
        let mut stream = response.into_body().into_data_stream();
        assert!(matches!(next(&mut stream).await, S::RunReady(_)));
        if id == "parent" && stop_between_calls {
            provider.push(vec![ModelEvent::Done(FinishReason::Stop)]);
        } else {
            provider.push_pending();
        }
        append(
            &app,
            id,
            1,
            C::InvokeModel(pb::RunInferenceInvokeModel {
                invocation_id: format!("{id}-call"),
                request: Some(pb::InferenceStreamRequest {
                    parent_request_id: parent
                        .filter(|_| lineage_source == LineageSource::Invocation)
                        .map(str::to_owned),
                    root_parent_request_id: root
                        .filter(|_| lineage_source == LineageSource::Invocation)
                        .map(str::to_owned),
                    ..Default::default()
                }),
                ..Default::default()
            }),
        )
        .await;
        assert!(matches!(next(&mut stream).await, S::InvocationResponse(_)));
        if id == "parent" && stop_between_calls {
            loop {
                if let S::InvocationEnd(end) = next(&mut stream).await {
                    assert!(end.error.is_none());
                    break;
                }
            }
        }
        streams.push(stream);
    }
    append(
        &app,
        "parent",
        2,
        C::FinishRun(pb::RunInferenceFinishRun {
            aborted: true,
            ..Default::default()
        }),
    )
    .await;
    for index in [1, 2] {
        assert_user_abort(next(&mut streams[index]).await);
    }
    assert_eq!(provider.requests().len(), 4);
    // A cancelled child must not start another provider call, even if Cursor
    // retries it while the independent client-side Agent is winding down.
    append(
        &app,
        "child",
        2,
        C::InvokeModel(pb::RunInferenceInvokeModel {
            invocation_id: "child-retry".into(),
            request: Some(pb::InferenceStreamRequest {
                parent_request_id: (lineage_source == LineageSource::Invocation)
                    .then(|| "parent".into()),
                ..Default::default()
            }),
            ..Default::default()
        }),
    )
    .await;
    assert_user_abort(next(&mut streams[1]).await);
    // A child first arriving after its parent stopped is also rejected.
    let late_headers = [("x-root-parent-request-id", parent_id)];
    append_with_headers(
        &app,
        "late-child",
        0,
        C::RunRequest(pb::RunInferenceRunRequest {
            conversation_id: "conversation-late-child".into(),
            requested_model: Some(pb::InferenceRequestedModel {
                model_id: model.model_hash.clone(),
                ..Default::default()
            }),
            ..Default::default()
        }),
        if matches!(
            lineage_source,
            LineageSource::AppendHeaders | LineageSource::RetryHeaders
        ) {
            &late_headers
        } else {
            &[]
        },
    )
    .await;
    let response = post_with_headers(
        &app,
        "/aiserver.v1.InferenceService/RunInferenceSSE",
        &agent::BidiRequestId {
            request_id: "late-child".into(),
        },
        if lineage_source == LineageSource::SseHeaders {
            &late_headers
        } else {
            &[]
        },
    )
    .await;
    let mut late = response.into_body().into_data_stream();
    assert!(matches!(next(&mut late).await, S::RunReady(_)));
    append(
        &app,
        "late-child",
        1,
        C::InvokeModel(pb::RunInferenceInvokeModel {
            invocation_id: "late-call".into(),
            request: Some(pb::InferenceStreamRequest {
                parent_request_id: (lineage_source == LineageSource::Invocation)
                    .then(|| "parent".into()),
                ..Default::default()
            }),
            ..Default::default()
        }),
    )
    .await;
    assert_user_abort(next(&mut late).await);
    assert_eq!(provider.requests().len(), 4);
    // The unrelated stream is still pending, rather than cancelled by parent.
    assert!(
        tokio::time::timeout(Duration::from_millis(80), next(&mut streams[3]))
            .await
            .is_err()
    );
    append(
        &app,
        "unrelated",
        2,
        C::CancelInvocation(pb::RunInferenceCancelInvocation {
            invocation_id: "unrelated-call".into(),
        }),
    )
    .await;
    assert!(
        matches!(next(&mut streams[3]).await, S::InvocationEnd(end) if end.error.as_ref().is_some_and(|error| error.code == 1))
    );
    for (id, seq) in [
        ("child", 3),
        ("grandchild", 2),
        ("unrelated", 3),
        ("late-child", 2),
    ] {
        append(
            &app,
            id,
            seq,
            C::FinishRun(pb::RunInferenceFinishRun::default()),
        )
        .await;
    }
    // A new turn in the parent's conversation must remain usable.
    append(
        &app,
        "new-turn",
        0,
        C::RunRequest(pb::RunInferenceRunRequest {
            conversation_id: "conversation-parent".into(),
            previous_request_id: Some("parent".into()),
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
            request_id: "new-turn".into(),
        },
    )
    .await;
    let mut resumed = response.into_body().into_data_stream();
    assert!(matches!(next(&mut resumed).await, S::RunReady(_)));
    provider.push(vec![ModelEvent::Done(FinishReason::Stop)]);
    append(
        &app,
        "new-turn",
        1,
        C::InvokeModel(pb::RunInferenceInvokeModel {
            invocation_id: "resumed".into(),
            request: Some(pb::InferenceStreamRequest::default()),
            ..Default::default()
        }),
    )
    .await;
    assert!(matches!(next(&mut resumed).await, S::InvocationResponse(_)));
    loop {
        if let S::InvocationEnd(end) = next(&mut resumed).await {
            assert!(end.error.is_none());
            break;
        }
    }
    append(
        &app,
        "new-turn",
        2,
        C::FinishRun(pb::RunInferenceFinishRun::default()),
    )
    .await;
}

#[tokio::test]
async fn stopping_parent_cancels_running_and_late_descendants_only() {
    assert_parent_stop_cancels_descendants(false, LineageSource::Invocation).await;
}

#[tokio::test]
async fn stopping_parent_between_model_calls_also_cancels_descendants() {
    assert_parent_stop_cancels_descendants(true, LineageSource::Invocation).await;
}

#[tokio::test]
async fn stopping_parent_cancels_children_linked_only_by_append_headers() {
    assert_parent_stop_cancels_descendants(false, LineageSource::AppendHeaders).await;
}

#[tokio::test]
async fn stopping_parent_cancels_children_linked_only_by_sse_headers() {
    assert_parent_stop_cancels_descendants(true, LineageSource::SseHeaders).await;
}

#[tokio::test]
async fn stopping_a_retried_parent_cancels_children_of_its_original_request() {
    assert_parent_stop_cancels_descendants(true, LineageSource::RetryHeaders).await;
}

#[tokio::test]
async fn normal_parent_finish_does_not_cancel_background_child() {
    use pb::run_inference_client_message::Message as C;
    use pb::run_inference_server_message::Message as S;
    let (_dir, store) = fixtures::temp_store().await;
    let model = store.create_model(&model_input()).await.unwrap();
    let provider = Arc::new(fake_provider::FakeProvider::default());
    let assets =
        PromptAssets::load(&std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("prompt/cursor"))
            .unwrap();
    let registry =
        TransportRegistry::new(store.clone(), provider.clone(), PromptCompiler::new(assets));
    let app = cursor::router(registry, NetworkClients::new(store)).unwrap();
    let mut streams = Vec::new();
    for id in ["parent", "child"] {
        append(
            &app,
            id,
            0,
            C::RunRequest(pb::RunInferenceRunRequest {
                conversation_id: id.into(),
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
        let mut stream = response.into_body().into_data_stream();
        assert!(matches!(next(&mut stream).await, S::RunReady(_)));
        provider.push_pending();
        append(
            &app,
            id,
            1,
            C::InvokeModel(pb::RunInferenceInvokeModel {
                invocation_id: id.into(),
                request: Some(pb::InferenceStreamRequest {
                    parent_request_id: (id == "child").then(|| "parent".into()),
                    ..Default::default()
                }),
                ..Default::default()
            }),
        )
        .await;
        assert!(matches!(next(&mut stream).await, S::InvocationResponse(_)));
        streams.push(stream);
    }
    // CancelInvocation alone is model-level cancellation, not an explicit
    // abort of the entire Agent run. Normal finish must also leave children.
    append(
        &app,
        "parent",
        2,
        C::CancelInvocation(pb::RunInferenceCancelInvocation {
            invocation_id: "parent".into(),
        }),
    )
    .await;
    let S::InvocationEnd(end) = next(&mut streams[0]).await else {
        panic!("expected model-level cancellation");
    };
    let error = end.error.unwrap();
    assert_eq!(error.code, 1);
    assert_eq!(error.message, "run was cancelled");
    assert!(error.details.is_empty());
    append(
        &app,
        "parent",
        3,
        C::FinishRun(pb::RunInferenceFinishRun {
            turn_completed: Some(true),
            ..Default::default()
        }),
    )
    .await;
    provider.push(vec![ModelEvent::Done(FinishReason::Stop)]);
    append(
        &app,
        "child",
        2,
        C::InvokeModel(pb::RunInferenceInvokeModel {
            invocation_id: "child-next".into(),
            request: Some(pb::InferenceStreamRequest {
                parent_request_id: Some("parent".into()),
                ..Default::default()
            }),
            ..Default::default()
        }),
    )
    .await;
    assert!(matches!(
        next(&mut streams[1]).await,
        S::InvocationResponse(_)
    ));
    loop {
        if let S::InvocationEnd(end) = next(&mut streams[1]).await {
            assert!(end.error.is_none());
            break;
        }
    }
    append(
        &app,
        "child",
        3,
        C::FinishRun(pb::RunInferenceFinishRun::default()),
    )
    .await;
}

#[tokio::test]
async fn cursor_can_resume_after_stopping_a_partially_streamed_task() {
    use pb::run_inference_client_message::Message as C;
    use pb::run_inference_server_message::Message as S;
    let (_dir, store) = fixtures::temp_store().await;
    let model = store.create_model(&model_input()).await.unwrap();
    let provider = Arc::new(fake_provider::FakeProvider::default());
    provider.push(vec![
        ModelEvent::TextDelta("已收到停止后的补充".into()),
        ModelEvent::Done(FinishReason::Stop),
    ]);
    let assets =
        PromptAssets::load(&std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("prompt/cursor"))
            .unwrap();
    let registry =
        TransportRegistry::new(store.clone(), provider.clone(), PromptCompiler::new(assets));
    let app = cursor::router(registry, NetworkClients::new(store)).unwrap();
    let id = "resume-after-stop";
    append(
        &app,
        id,
        0,
        C::RunRequest(pb::RunInferenceRunRequest {
            conversation_id: "stopped-task-conversation".into(),
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
    assert!(matches!(next(&mut stream).await, S::RunReady(_)));
    append(
        &app,
        id,
        1,
        C::InvokeModel(pb::RunInferenceInvokeModel {
            invocation_id: "resume".into(),
            request: Some(pb::InferenceStreamRequest {
                messages: vec![
                    pb::InferenceCoreMessage {
                        role: 2,
                        tool_calls: vec![pb::InferenceToolCall {
                            tool_call_id: "stopped-task".into(),
                            tool_name: "Task".into(),
                            raw_tool_call_args: Some(r#"{"prompt":"只读研究尚未生成完成"#.into()),
                            ..Default::default()
                        }],
                        ..Default::default()
                    },
                    pb::InferenceCoreMessage {
                        role: 1,
                        content: Some(pb::inference_core_message::Content::Text(
                            "已停止旧任务，现在可以先执行两个。".into(),
                        )),
                        ..Default::default()
                    },
                ],
                ..Default::default()
            }),
            ..Default::default()
        }),
    )
    .await;
    loop {
        if let S::InvocationEnd(end) = next(&mut stream).await {
            assert!(end.error.is_none(), "{:?}", end.error);
            break;
        }
    }
    let requests = provider.requests();
    assert_eq!(requests.len(), 1);
    assert!(matches!(&requests[0].history[0].content,
        ProjectedContent::Assistant { calls, text, .. } if calls.is_empty() && text.contains("interrupted")));
    assert!(matches!(&requests[0].history[1].content,
        ProjectedContent::Parts(parts) if matches!(&parts[0], cursor_server::model::ContentPart::Text { text } if text.contains("可以先执行两个"))));
    append(
        &app,
        id,
        2,
        C::FinishRun(pb::RunInferenceFinishRun::default()),
    )
    .await;
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
        for selected in [model.model_hash.as_str(), "official-model"] {
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
