//! Cursor selections and request parameters must reach the provider unchanged.
#[path = "support/fixtures.rs"]
mod fixtures;

use axum::{extract::State, routing::post, Json, Router};
use cursor_server::{
    cursor::{
        inference::{invocation, prompt_metadata},
        protocol::proto::inference as pb,
    },
    model::{ModelConfigInput, ModelType, OPENAI_CHAT_ENDPOINT, OPENAI_RESPONSES_ENDPOINT},
    network::NetworkClients,
    provider::{Provider, ProviderRouter},
};
use futures_util::StreamExt;
use serde_json::{json, Value};
use std::{
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio_util::sync::CancellationToken;

#[tokio::test]
async fn selected_effort_and_runtime_sampling_override_saved_extra_parameters() {
    for (model_type, endpoint, extra, effort_path) in [
        (
            ModelType::OpenAi,
            OPENAI_CHAT_ENDPOINT,
            json!({"reasoning_effort":"low", "temperature":0.9}),
            "/reasoning_effort",
        ),
        (
            ModelType::OpenAi,
            OPENAI_RESPONSES_ENDPOINT,
            json!({"reasoning":{"effort":"low","summary":"auto"}, "temperature":0.9}),
            "/reasoning/effort",
        ),
        (
            ModelType::Anthropic,
            OPENAI_CHAT_ENDPOINT,
            json!({"output_config":{"effort":"low","format":{"type":"json_schema"}}, "thinking":{"type":"disabled"}, "temperature":0.9}),
            "/output_config/effort",
        ),
    ] {
        for explicit_effort in [true, false] {
            let mut extra = extra.clone();
            if !explicit_effort && model_type == ModelType::OpenAi {
                extra.as_object_mut().unwrap().remove("reasoning_effort");
                extra.as_object_mut().unwrap().remove("reasoning");
            }
            let captured = Arc::new(Mutex::new(Vec::<Value>::new()));
            let response = match (model_type, endpoint) {
                (ModelType::Anthropic, _) => "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"}}\n\ndata: {\"type\":\"message_stop\"}\n\n",
                (_, OPENAI_RESPONSES_ENDPOINT) => "data: {\"type\":\"response.completed\",\"response\":{\"output\":[]}}\n\n",
                _ => "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
            };
            let app =
                Router::new()
                    .route(
                        "/",
                        post(
                            move |State(c): State<Arc<Mutex<Vec<Value>>>>,
                                  Json(body): Json<Value>| async move {
                                c.lock().unwrap().push(body);
                                ([("content-type", "text/event-stream")], response)
                            },
                        ),
                    )
                    .with_state(captured.clone());
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap();
            let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
            let (_dir, store) = fixtures::temp_store().await;
            let input: ModelConfigInput = serde_json::from_value(json!({
            "display_name":"Parameter test", "type":model_type, "base_url":format!("http://{addr}/"), "use_full_url":true,
            "api_key":"synthetic-test-key", "tooltip_data":"Test", "model_id":"synthetic-model", "openai_endpoint":endpoint,
            "openai_extra_params_enabled":true, "openai_extra_params":extra,
            "anthropic_extra_params_enabled":true, "anthropic_extra_params":extra,
            "reasoning_effort": if explicit_effort { Some("medium") } else { None },
            "anthropic_thinking_effort":"medium"
        })).unwrap();
            let model = store.create_model(&input).await.unwrap();
            let selected = pb::InferenceRequestedModel {
                model_id: model.model_hash.clone(),
                parameters: std::iter::once(pb::InferenceModelParameterValue {
                    id: "context".into(),
                    value: "1m".into(),
                })
                .chain(explicit_effort.then(|| pb::InferenceModelParameterValue {
                    id: "reasoning".into(),
                    value: "high".into(),
                }))
                .collect(),
                ..Default::default()
            };
            let metadata = prompt_metadata(
                &pb::RunInferenceRunRequest {
                    requested_model: Some(selected.clone()),
                    ..Default::default()
                },
                Some(&model),
                &[model.clone()],
            );
            if explicit_effort {
                assert_eq!(metadata.reasoning_effort.as_deref(), Some("high"));
            }
            assert_eq!(metadata.agent_token_limit, Some(1_000_000));
            let req = pb::InferenceStreamRequest {
                requested_model: Some(selected),
                model_config: Some(pb::InferenceModelConfig {
                    temperature: Some(0.25),
                    ..Default::default()
                }),
                ..Default::default()
            };
            let input = invocation(
                &req,
                &model.model_hash,
                "synthetic-session",
                "synthetic-run",
                "sample",
                0,
            )
            .unwrap();
            let provider = ProviderRouter::new(
                store.clone(),
                NetworkClients::new(store),
                Duration::from_secs(5),
                Duration::from_secs(5),
            );
            let mut stream = provider.stream(input, CancellationToken::new());
            let mut done = false;
            while let Some(event) = stream.next().await {
                if matches!(event.unwrap(), cursor_server::provider::ModelEvent::Done(_)) {
                    done = true;
                }
            }
            assert!(done, "the simulated provider must complete successfully");
            server.abort();
            let requests = captured.lock().unwrap();
            assert_eq!(requests.len(), 1);
            let body = &requests[0];
            if explicit_effort {
                assert_eq!(
                    body.pointer(effort_path),
                    Some(&json!("high")),
                    "{model_type:?} {endpoint}: metadata and actual effort disagree"
                );
            } else if model_type == ModelType::OpenAi {
                assert!(body.get("reasoning_effort").is_none());
                assert!(body.get("reasoning").is_none());
            }
            assert_eq!(
                body["temperature"],
                json!(0.25),
                "runtime sampling was overwritten by saved extras"
            );
            if model_type == ModelType::Anthropic {
                if explicit_effort {
                    assert_ne!(body.pointer("/thinking/type"), Some(&json!("disabled")));
                } else {
                    assert_eq!(body.pointer("/thinking/type"), Some(&json!("disabled")));
                }
                assert_eq!(
                    body.pointer("/output_config/format/type"),
                    Some(&json!("json_schema"))
                );
            }
        }
    }
}
