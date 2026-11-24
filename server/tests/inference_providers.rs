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
    provider::{build_provider, Provider},
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

async fn mock_provider(
    kind: ProviderKind,
    responses: impl IntoIterator<Item = String>,
) -> (Arc<dyn Provider>, Mock, tokio::task::JoinHandle<()>) {
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
    (provider, mock, server)
}

async fn http_inference(
    kind: ProviderKind,
    request: pb::InferenceStreamRequest,
    response: String,
) -> (Value, Vec<pb::InferenceStreamResponse>) {
    let (provider, mock, server) = mock_provider(kind, [response]).await;
    let request = pb::InferenceStreamRequest::decode(request.encode_to_vec().as_slice()).unwrap();
    let input = invocation(&request, "test-model", "conversation", "run", "call", 0).unwrap();
    let mut stream = provider.stream(input, CancellationToken::new());
    let mut output = InvocationOutput::default();
    let mut responses = Vec::new();
    while let Some(event) = tokio::time::timeout(Duration::from_secs(5), stream.next())
        .await
        .unwrap()
    {
        for response in output.event(event.unwrap(), "test-model").unwrap() {
            responses.push(
                pb::InferenceStreamResponse::decode(response.encode_to_vec().as_slice()).unwrap(),
            );
        }
    }
    server.abort();
    assert!(output.done);
    let requests = mock.requests.lock().unwrap();
    assert_eq!(requests.len(), 1);
    (requests[0].clone(), responses)
}

#[tokio::test]
async fn cached_context_usage_reaches_cursor_for_all_three_protocols() {
    #[derive(Clone, PartialEq, Message)]
    struct CurrentContextUsage {
        #[prost(int32, optional, tag = "7")]
        context_input_tokens: Option<i32>,
    }

    for (kind, response, billed_input) in [
        (
            ProviderKind::OpenAiChat,
            sse(vec![json!({
                "choices":[{"delta":{"content":"OK"},"finish_reason":"stop"}],
                "usage":{"prompt_tokens":100766,"completion_tokens":20,"total_tokens":100786,
                    "prompt_tokens_details":{"cached_tokens":100352}}
            })]),
            100766,
        ),
        (
            ProviderKind::OpenAiResponses,
            sse(vec![json!({"type":"response.completed","response":{
                "output":[],"usage":{"input_tokens":100766,"output_tokens":20,
                    "total_tokens":100786,"input_tokens_details":{"cached_tokens":100352}}
            }})]),
            100766,
        ),
        (
            ProviderKind::Anthropic,
            sse(vec![
                json!({"type":"message_start","message":{"usage":{
                    "input_tokens":414,"cache_read_input_tokens":100000,
                    "cache_creation_input_tokens":352,"output_tokens":0
                }}}),
                json!({"type":"message_delta","delta":{"stop_reason":"end_turn"},
                    "usage":{"output_tokens":20}}),
                json!({"type":"message_stop"}),
            ]),
            414,
        ),
    ] {
        let (_, responses) = http_inference(kind, Default::default(), response).await;
        let usage = responses
            .into_iter()
            .filter_map(|r| match r.response {
                Some(pb::inference_stream_response::Response::ExtendedUsage(u)) => Some(u),
                _ => None,
            })
            .last()
            .unwrap();
        assert_eq!(usage.input_tokens, billed_input);
        assert_eq!(usage.output_tokens, 20);
        let wire = CurrentContextUsage::decode(usage.encode_to_vec().as_slice()).unwrap();
        assert_eq!(wire.context_input_tokens, Some(100766));
    }
}

async fn round_trip(kind: ProviderKind, responses: [String; 2]) -> Vec<Value> {
    let (provider, mock, server) = mock_provider(kind, responses).await;
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
        // Stopping Cursor while the next Task is streaming leaves incomplete
        // arguments in history. All providers must still accept the next turn,
        // including the completed Read and its native reasoning replay above.
        request.messages.extend([
            pb::InferenceCoreMessage {
                role: 2,
                tool_calls: vec![pb::InferenceToolCall {
                    tool_call_id: "interrupted-task".into(),
                    tool_name: "Task".into(),
                    raw_tool_call_args: Some(r#"{"prompt":"unfinished-task-arguments"#.into()),
                    ..Default::default()
                }],
                ..Default::default()
            },
            pb::InferenceCoreMessage {
                role: 3,
                content: Some(pb::inference_core_message::Content::ToolContent(
                    pb::InferenceToolResultContent {
                        parts: vec![pb::InferenceToolResultPart {
                            tool_call_id: "interrupted-task".into(),
                            tool_name: "Task".into(),
                            result: Some(string("Tool call arguments were not valid JSON")),
                            cursor_tool_call_is_error: Some(true),
                            ..Default::default()
                        }],
                    },
                )),
                ..Default::default()
            },
            pb::InferenceCoreMessage {
                role: 1,
                content: Some(pb::inference_core_message::Content::Text(
                    "Continue after stopping. Do not restart the interrupted Task.".into(),
                )),
                ..Default::default()
            },
        ]);
    }
    server.abort();
    let requests = mock.requests.lock().unwrap().clone();
    let resumed = requests[1].to_string();
    assert!(resumed.contains("Continue after stopping"));
    assert!(resumed.contains("interrupted"));
    assert!(!resumed.contains("unfinished-task-arguments"));
    for field in ["id", "call_id", "tool_call_id", "tool_use_id"] {
        assert!(!resumed.contains(&format!("\"{field}\":\"interrupted-task\"")));
    }
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

#[tokio::test]
async fn anthropic_http_preserves_cursor_tool_result_error_flags() {
    use pb::inference_core_message::Content;
    for (is_error, cursor_error) in [
        (true, None),
        (true, Some(false)),
        (false, Some(true)),
        (true, Some(true)),
        (false, Some(false)),
        (false, None),
    ] {
        let request = pb::InferenceStreamRequest {
            tools: vec![pb::InferenceAgentTool {
                name: "Read".into(),
                ..Default::default()
            }],
            messages: vec![
                pb::InferenceCoreMessage {
                    role: 1,
                    content: Some(Content::Text("Read a.txt".into())),
                    ..Default::default()
                },
                pb::InferenceCoreMessage {
                    role: 2,
                    tool_calls: vec![pb::InferenceToolCall {
                        tool_call_id: "tool-1".into(),
                        tool_name: "Read".into(),
                        raw_tool_call_args: Some(r#"{"path":"a.txt"}"#.into()),
                        ..Default::default()
                    }],
                    ..Default::default()
                },
                pb::InferenceCoreMessage {
                    role: 3,
                    content: Some(Content::ToolContent(pb::InferenceToolResultContent {
                        parts: vec![pb::InferenceToolResultPart {
                            tool_call_id: "tool-1".into(),
                            tool_name: "Read".into(),
                            result: Some(prost_types::Value {
                                kind: Some(prost_types::value::Kind::StringValue(
                                    "tool output".into(),
                                )),
                            }),
                            is_error,
                            cursor_tool_call_is_error: cursor_error,
                            ..Default::default()
                        }],
                    })),
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        let (body, responses) = http_inference(
            ProviderKind::Anthropic,
            request,
            sse(vec![
                json!({"type":"message_delta","delta":{"stop_reason":"end_turn"}}),
                json!({"type":"message_stop"}),
            ]),
        )
        .await;
        let blocks: Vec<_> = body["messages"]
            .as_array()
            .unwrap()
            .iter()
            .flat_map(|message| message["content"].as_array().unwrap())
            .collect();
        let calls: Vec<_> = blocks
            .iter()
            .filter(|block| block["type"] == "tool_use")
            .collect();
        let results: Vec<_> = blocks
            .iter()
            .filter(|block| block["type"] == "tool_result")
            .collect();
        assert_eq!(calls.len(), 1);
        assert_eq!(results.len(), 1);
        assert_eq!(calls[0]["id"], "tool-1");
        assert_eq!(results[0]["tool_use_id"], "tool-1");
        assert_eq!(results[0]["content"], "tool output");
        assert_eq!(
            results[0].get("is_error").cloned().unwrap_or(json!(false)),
            json!(is_error || cursor_error.unwrap_or(false)),
            "is_error={is_error}, cursor_tool_call_is_error={cursor_error:?}"
        );
        assert!(!responses.iter().any(|response| matches!(
            &response.response,
            Some(pb::inference_stream_response::Response::Error(_))
        )));
    }
}

async fn assert_http_output_token_limit(kind: ProviderKind, response: String) {
    use pb::inference_stream_response::Response as R;
    let request = pb::InferenceStreamRequest {
        messages: vec![pb::InferenceCoreMessage {
            role: 1,
            content: Some(pb::inference_core_message::Content::Text("Continue".into())),
            ..Default::default()
        }],
        ..Default::default()
    };
    let (_, responses) = http_inference(kind, request, response).await;
    let text: String = responses
        .iter()
        .filter_map(|response| match &response.response {
            Some(R::TextPart(part)) => Some(part.text.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(text, "partial answer");
    let infos: Vec<_> = responses
        .iter()
        .filter_map(|response| match &response.response {
            Some(R::ResponseInfo(info)) => Some(info),
            _ => None,
        })
        .collect();
    assert_eq!(infos.len(), 1);
    assert_eq!(
        infos[0].error_message.as_deref(),
        Some("Provider exceeded max output tokens")
    );
    assert_eq!(infos[0].messages.len(), 1);
    assert_eq!(
        infos[0].messages[0].content.as_deref(),
        Some("partial answer")
    );
    let errors: Vec<_> = responses
        .iter()
        .filter_map(|response| match &response.response {
            Some(R::Error(error)) => Some(error),
            _ => None,
        })
        .collect();
    assert_eq!(errors.len(), 1);
    assert_eq!(
        infos[0].error_message.as_deref(),
        Some(errors[0].message.as_str())
    );
    assert!(errors[0].is_output_token_limit_error);
    assert!(!errors[0].is_input_token_limit_error);
    assert_eq!(
        errors[0].error_type,
        pb::InferenceStreamErrorType::InferencestreamerrortypeOutputTokenLimit as i32
    );
}

#[tokio::test]
async fn chat_http_output_token_limit_preserves_partial_text_and_typed_error() {
    assert_http_output_token_limit(
        ProviderKind::OpenAiChat,
        format!(
            "{}data: [DONE]\n\n",
            sse(vec![
                json!({"choices":[{"delta":{"content":"partial "},"finish_reason":null}]}),
                json!({"choices":[{"delta":{"content":"answer"},"finish_reason":null}]}),
                json!({"choices":[{"delta":{},"finish_reason":"length"}]}),
            ])
        ),
    )
    .await;
}

#[tokio::test]
async fn responses_http_output_token_limit_preserves_partial_text_and_typed_error() {
    assert_http_output_token_limit(
        ProviderKind::OpenAiResponses,
        sse(vec![
            json!({"type":"response.output_text.delta","delta":"partial "}),
            json!({"type":"response.output_text.delta","delta":"answer"}),
            json!({"type":"response.incomplete","response":{"status":"incomplete","incomplete_details":{"reason":"max_output_tokens"}}}),
        ]),
    )
    .await;
}

#[tokio::test]
async fn anthropic_http_output_token_limit_preserves_partial_text_and_typed_error() {
    assert_http_output_token_limit(
        ProviderKind::Anthropic,
        sse(vec![
            json!({"type":"message_start","message":{"id":"msg-test","type":"message","role":"assistant","content":[]}}),
            json!({"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}),
            json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"partial "}}),
            json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"answer"}}),
            json!({"type":"content_block_stop","index":0}),
            json!({"type":"message_delta","delta":{"stop_reason":"max_tokens"}}),
            json!({"type":"message_stop"}),
        ]),
    )
    .await;
}

#[tokio::test]
async fn responses_multiple_text_parts_reconcile_independently_without_duplicates() {
    let (_, responses) = http_inference(ProviderKind::OpenAiResponses, pb::InferenceStreamRequest::default(), sse(vec![
        json!({"type":"response.output_text.delta","output_index":0,"content_index":0,"delta":"前"}),
        json!({"type":"response.output_text.done","output_index":0,"content_index":0,"text":"前段"}),
        json!({"type":"response.output_text.done","output_index":0,"content_index":1,"text":"中段"}),
        json!({"type":"response.output_item.done","output_index":0,"item":{"type":"message","content":[{"type":"output_text","text":"前段"},{"type":"output_text","text":"中段"}]}}),
        json!({"type":"response.output_text.delta","output_index":1,"content_index":0,"delta":"后"}),
        json!({"type":"response.output_item.done","output_index":1,"item":{"type":"message","content":[{"type":"output_text","text":"后段"}]}}),
        json!({"type":"response.completed","response":{}}),
    ])).await;
    let streamed: String = responses
        .iter()
        .filter_map(|r| match &r.response {
            Some(pb::inference_stream_response::Response::TextPart(p)) if !p.is_final => {
                Some(p.text.as_str())
            }
            _ => None,
        })
        .collect();
    assert_eq!(streamed, "前段中段后段");
    let info = responses
        .iter()
        .find_map(|r| match &r.response {
            Some(pb::inference_stream_response::Response::ResponseInfo(info)) => Some(info),
            _ => None,
        })
        .unwrap();
    assert_eq!(info.messages[0].content.as_deref(), Some("前段中段后段"));
}

#[tokio::test]
async fn truncated_tool_arguments_report_output_limit_without_executing_the_tool() {
    for (kind, events) in [
        (
            ProviderKind::OpenAiChat,
            vec![
                json!({"choices":[{"delta":{"content":"partial answer","tool_calls":[{"index":0,"id":"partial-tool","function":{"name":"Write","arguments":"{\"content\":\"unfinished"}}]},"finish_reason":null}]}),
                json!({"choices":[{"delta":{},"finish_reason":"length"}]}),
            ],
        ),
        (
            ProviderKind::OpenAiResponses,
            vec![
                json!({"type":"response.output_text.delta","delta":"partial answer"}),
                json!({"type":"response.output_item.added","output_index":1,"item":{"type":"function_call","call_id":"partial-tool","name":"Write"}}),
                json!({"type":"response.function_call_arguments.delta","output_index":1,"delta":"{\"content\":\"unfinished"}),
                json!({"type":"response.function_call_arguments.done","output_index":1,"arguments":"{\"content\":\"unfinished"}),
                json!({"type":"response.incomplete","response":{"incomplete_details":{"reason":"max_output_tokens"}}}),
            ],
        ),
    ] {
        let (_, responses) =
            http_inference(kind, pb::InferenceStreamRequest::default(), sse(events)).await;
        assert!(responses.iter().any(|r| matches!(&r.response, Some(pb::inference_stream_response::Response::Error(e)) if e.is_output_token_limit_error)));
        assert!(!responses.iter().any(|r| matches!(&r.response, Some(pb::inference_stream_response::Response::ToolCallPart(p)) if p.is_complete)));
        let info = responses
            .iter()
            .find_map(|r| match &r.response {
                Some(pb::inference_stream_response::Response::ResponseInfo(info)) => Some(info),
                _ => None,
            })
            .unwrap();
        assert_eq!(info.messages[0].content.as_deref(), Some("partial answer"));
        assert!(info.messages[0].tool_calls.is_empty());
    }
}

#[tokio::test]
async fn responses_mixed_content_order_survives_cursor_wire_and_next_request() {
    use pb::inference_core_message::Content;
    let (_, responses) = http_inference(ProviderKind::OpenAiResponses, pb::InferenceStreamRequest::default(), sse(vec![
        json!({"type":"response.output_item.done","output_index":0,"item":{"type":"reasoning","id":"rs-1","summary":[],"encrypted_content":"first-opaque"}}),
        json!({"type":"response.output_item.done","output_index":1,"item":{"type":"message","content":[{"type":"output_text","text":"before"}]}}),
        json!({"type":"response.output_item.done","output_index":2,"item":{"type":"function_call","call_id":"read-1","name":"Read","arguments":"{\"path\":\"a.txt\"}"}}),
        json!({"type":"response.output_item.done","output_index":3,"item":{"type":"reasoning","id":"rs-2","summary":[],"encrypted_content":"second-opaque"}}),
        json!({"type":"response.output_item.done","output_index":4,"item":{"type":"message","content":[{"type":"output_text","text":"after"}]}}),
        json!({"type":"response.completed","response":{}}),
    ])).await;
    let assistant = responses
        .into_iter()
        .find_map(|r| match r.response {
            Some(pb::inference_stream_response::Response::ResponseInfo(info)) => {
                info.messages.into_iter().next()
            }
            _ => None,
        })
        .unwrap();
    assert_eq!(assistant.assistant_part_order, vec![1, 2, 3, 1, 2]);
    assert_eq!(assistant.assistant_text_parts, vec!["before", "after"]);
    let request = pb::InferenceStreamRequest {
        messages: vec![
            pb::InferenceCoreMessage {
                role: 2,
                content: assistant.content.map(Content::Text),
                tool_calls: assistant.tool_calls,
                reasoning_parts: assistant.reasoning_parts,
                assistant_part_order: assistant.assistant_part_order,
                assistant_text_parts: assistant.assistant_text_parts,
                ..Default::default()
            },
            pb::InferenceCoreMessage {
                role: 3,
                content: Some(Content::ToolContent(pb::InferenceToolResultContent {
                    parts: vec![pb::InferenceToolResultPart {
                        tool_call_id: "read-1".into(),
                        tool_name: "Read".into(),
                        ..Default::default()
                    }],
                })),
                ..Default::default()
            },
        ],
        ..Default::default()
    };
    let (body, _) = http_inference(
        ProviderKind::OpenAiResponses,
        request,
        sse(vec![
            json!({"type":"response.output_text.delta","delta":"OK"}),
            json!({"type":"response.completed","response":{}}),
        ]),
    )
    .await;
    let input = body["input"].as_array().unwrap();
    let types: Vec<_> = input.iter().map(|v| v["type"].as_str().unwrap()).collect();
    assert_eq!(
        types,
        vec![
            "reasoning",
            "message",
            "function_call",
            "reasoning",
            "message",
            "function_call_output"
        ]
    );
    assert_eq!(input[0]["encrypted_content"], "first-opaque");
    assert_eq!(input[1]["content"][0]["text"], "before");
    assert_eq!(input[3]["encrypted_content"], "second-opaque");
    assert_eq!(input[4]["content"][0]["text"], "after");
}
