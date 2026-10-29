//! Tests all three HTTP provider adapters through Cursor's new message conversion.
use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
    time::Duration,
};

use axum::{extract::State, routing::post, Json, Router};
use cursor_server::{
    config::{ProviderConfig, ProviderKind},
    cursor::{
        inference::{invocation, InvocationOutput},
        protocol::proto::inference as pb,
    },
    provider::build_provider,
};
use futures_util::StreamExt;
use prost::Message;
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;

#[derive(Clone)]
struct Mock {
    requests: Arc<Mutex<Vec<Value>>>,
    responses: Arc<Mutex<VecDeque<String>>>,
}

fn sse(events: Vec<Value>) -> String {
    events
        .into_iter()
        .map(|event| format!("data: {event}\n\n"))
        .collect()
}

async fn round_trip(kind: ProviderKind, responses: [String; 2]) -> Vec<Value> {
    let mock = Mock {
        requests: Arc::default(),
        responses: Arc::new(Mutex::new(responses.into_iter().collect())),
    };
    let app = Router::new()
        .route(
            "/",
            post(
                |State(mock): State<Mock>, Json(body): Json<Value>| async move {
                    mock.requests.lock().unwrap().push(body);
                    let response = mock.responses.lock().unwrap().pop_front().unwrap();
                    ([("content-type", "text/event-stream")], response)
                },
            ),
        )
        .with_state(mock.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let provider = build_provider(&ProviderConfig {
        kind,
        request_url: format!("http://{addr}/"),
        api_key: "test-key".into(),
        custom_headers: Default::default(),
        max_output_tokens: Some(2048),
        request_timeout: Duration::from_secs(5),
        allowed_body_fields: None,
    })
    .unwrap();
    use prost_types::{value::Kind, Struct, Value as ProtoValue};
    let string = |v: &str| ProtoValue {
        kind: Some(Kind::StringValue(v.into())),
    };
    let object = |fields| ProtoValue {
        kind: Some(Kind::StructValue(Struct { fields })),
    };
    let parameters = Struct {
        fields: [(
            "jsonSchema".into(),
            object(
                [
                    ("type".into(), string("object")),
                    (
                        "properties".into(),
                        object(
                            [(
                                "path".into(),
                                object(
                                    [
                                        ("type".into(), string("string")),
                                        (
                                            "minLength".into(),
                                            ProtoValue {
                                                kind: Some(Kind::NumberValue(1.0)),
                                            },
                                        ),
                                    ]
                                    .into(),
                                ),
                            )]
                            .into(),
                        ),
                    ),
                ]
                .into(),
            ),
        )]
        .into(),
    };
    let mut request = pb::InferenceStreamRequest {
        tools: vec![pb::InferenceAgentTool {
            name: "Read".into(),
            parameters: Some(parameters),
            ..Default::default()
        }],
        messages: vec![pb::InferenceCoreMessage {
            role: 1,
            content: Some(pb::inference_core_message::Content::Text(
                "Read a.txt".into(),
            )),
            ..Default::default()
        }],
        ..Default::default()
    };
    // Include actual protobuf encoding so numeric/schema and replay bugs are exercised.
    request = pb::InferenceStreamRequest::decode(request.encode_to_vec().as_slice()).unwrap();
    for index in 0..2 {
        let input =
            invocation(&request, "test-model", "conversation", "run", "call", index).unwrap();
        let mut stream = provider.stream(input, CancellationToken::new());
        let mut output = InvocationOutput::default();
        let mut assistant = None;
        while let Some(event) = tokio::time::timeout(Duration::from_secs(5), stream.next())
            .await
            .unwrap()
        {
            for response in output.event(event.unwrap(), "test-model").unwrap() {
                let response =
                    pb::InferenceStreamResponse::decode(response.encode_to_vec().as_slice())
                        .unwrap();
                if let Some(pb::inference_stream_response::Response::ResponseInfo(info)) =
                    response.response
                {
                    assistant = info.messages.into_iter().next();
                }
            }
        }
        assert!(output.done);
        let assistant = assistant.unwrap();
        if index == 1 {
            assert_eq!(assistant.content.as_deref(), Some("OK"));
            break;
        }
        assert_eq!(assistant.tool_calls[0].tool_call_id, "tool-1");
        request.messages.push(pb::InferenceCoreMessage {
            role: 2,
            content: assistant
                .content
                .map(pb::inference_core_message::Content::Text),
            tool_calls: assistant.tool_calls,
            reasoning_parts: assistant.reasoning_parts,
            anthropic_native_content: assistant.anthropic_native_content,
            ..Default::default()
        });
        request.messages.push(pb::InferenceCoreMessage {
            role: 3,
            content: Some(pb::inference_core_message::Content::ToolContent(
                pb::InferenceToolResultContent {
                    parts: vec![pb::InferenceToolResultPart {
                        tool_call_id: "tool-1".into(),
                        tool_name: "Read".into(),
                        result: Some(string("file contents")),
                        ..Default::default()
                    }],
                },
            )),
            ..Default::default()
        });
    }
    server.abort();
    let requests = mock.requests.lock().unwrap().clone();
    let schema = if requests[0]["tools"][0]["function"].is_object() {
        &requests[0]["tools"][0]["function"]["parameters"]
    } else if requests[0]["tools"][0].get("input_schema").is_some() {
        &requests[0]["tools"][0]["input_schema"]
    } else {
        &requests[0]["tools"][0]["parameters"]
    };
    assert_eq!(schema["properties"]["path"]["minLength"].as_i64(), Some(1));
    requests
}

#[tokio::test]
async fn chat_completions_keeps_reasoning_and_tool_results() {
    let requests = round_trip(ProviderKind::OpenAiChat, [sse(vec![
        json!({"choices":[{"delta":{"reasoning_content":"thought","tool_calls":[{"index":0,"id":"tool-1","function":{"name":"Read","arguments":"{\"path\":\"a.txt\"}"}}]},"finish_reason":null}]}),
        json!({"choices":[{"delta":{},"finish_reason":"tool_calls"}]}),
    ]), sse(vec![json!({"choices":[{"delta":{"content":"OK"},"finish_reason":"stop"}]})])]).await;
    assert_eq!(requests[1]["messages"][1]["reasoning_content"], "thought");
    assert_eq!(requests[1]["messages"][2]["tool_call_id"], "tool-1");
}

#[tokio::test]
async fn responses_keeps_encrypted_reasoning_and_tool_results() {
    let requests = round_trip(ProviderKind::OpenAiResponses, [sse(vec![
        json!({"type":"response.output_item.done","output_index":0,"item":{"type":"reasoning","id":"rs-1","summary":[],"encrypted_content":"opaque"}}),
        json!({"type":"response.output_item.done","output_index":1,"item":{"type":"function_call","id":"fc-1","call_id":"tool-1","name":"Read","arguments":"{\"path\":\"a.txt\"}"}}),
        json!({"type":"response.completed","response":{}}),
    ]), sse(vec![
        json!({"type":"response.output_text.delta","delta":"OK"}),
        json!({"type":"response.completed","response":{}}),
    ])]).await;
    let input = requests[1]["input"].as_array().unwrap();
    assert!(input
        .iter()
        .any(|i| i["type"] == "reasoning" && i["encrypted_content"] == "opaque"));
    assert!(input
        .iter()
        .any(|i| i["type"] == "function_call_output" && i["call_id"] == "tool-1"));
}

#[tokio::test]
async fn anthropic_keeps_signed_thinking_and_tool_results() {
    let requests = round_trip(ProviderKind::Anthropic, [sse(vec![
        json!({"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":""}}),
        json!({"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"thought"}}),
        json!({"type":"content_block_delta","index":0,"delta":{"type":"signature_delta","signature":"signed"}}),
        json!({"type":"content_block_stop","index":0}),
        json!({"type":"content_block_start","index":1,"content_block":{"type":"tool_use","id":"tool-1","name":"Read","input":{}}}),
        json!({"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"{\"path\":\"a.txt\"}"}}),
        json!({"type":"content_block_stop","index":1}),
        json!({"type":"message_delta","delta":{"stop_reason":"tool_use"}}),
        json!({"type":"message_stop"}),
    ]), sse(vec![
        json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"OK"}}),
        json!({"type":"content_block_stop","index":0}),
        json!({"type":"message_delta","delta":{"stop_reason":"end_turn"}}),
        json!({"type":"message_stop"}),
    ])]).await;
    assert_eq!(
        requests[1]["messages"][1]["content"][0]["signature"],
        "signed"
    );
    assert_eq!(
        requests[1]["messages"][2]["content"][0]["tool_use_id"],
        "tool-1"
    );
}
