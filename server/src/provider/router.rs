//! Routes model requests to configured providers.
use std::{sync::Arc, time::Duration};

use async_stream::try_stream;
use futures_util::StreamExt;
use tokio_util::sync::CancellationToken;

use crate::{
    config::{ProviderConfig, ProviderKind},
    model::{ModelInvocation, ModelLatency, NewLlmCall, ProviderType},
    store::Store,
    Error, Result,
};

use super::{
    normalize::NormalizedProvider, AnthropicProvider, CallRecorder, OpenAiChatProvider,
    OpenAiResponsesProvider, Provider, ProviderStream,
};

pub struct ProviderRouter {
    store: Store,
    clients: crate::network::NetworkClients,
    request_timeout: Duration,
    stream_idle_timeout: Duration,
}

impl ProviderRouter {
    pub fn new(
        store: Store,
        clients: crate::network::NetworkClients,
        request_timeout: Duration,
        stream_idle_timeout: Duration,
    ) -> Self {
        Self {
            store,
            clients,
            request_timeout,
            stream_idle_timeout,
        }
    }
}

impl Provider for ProviderRouter {
    fn stream(
        &self,
        invocation: ModelInvocation,
        cancellation: CancellationToken,
    ) -> ProviderStream {
        let store = self.store.clone();
        let clients = self.clients.clone();
        let request_timeout = self.request_timeout;
        let stream_idle_timeout = self.stream_idle_timeout;
        Box::pin(try_stream! {
            let selected = invocation.request.model.model_id.clone();
            let mut routed = invocation.clone();
            let model = store.model(&selected).await?.ok_or_else(|| Error::Provider(format!("unknown model: {selected}")))?;
            if model.is_image_generation() {
                Err(Error::Config("image generation models cannot be used for chat or subagents".into()))?;
            }
            let provider_type = model.provider_type();
            let request_url = model.request_url()?;
            model.configure(&mut routed.request.model);
            let mut configured_params = super::request_template::render_json_strings(
                model.extra_params(),
                &invocation.conversation_id,
            );
            // Cursor's selected effort takes precedence over saved extras.
            // Omitted effort keeps the provider's configured behavior.
            if let Some(effort) = &invocation.request.model.reasoning.effort {
                let (key, nested) = match provider_type {
                    ProviderType::OpenAiChat => ("reasoning_effort", false),
                    ProviderType::OpenAiResponses => ("reasoning", true),
                    ProviderType::Anthropic => ("output_config", true),
                    ProviderType::Plugin | ProviderType::OpenAiImages => unreachable!("not a chat provider"),
                };
                if let Some(value) = configured_params.get_mut(key) {
                    if nested {
                        let object = value.as_object_mut().ok_or_else(|| Error::Config(format!("{key} extra params must be an object")))?;
                        object.insert("effort".into(), serde_json::json!(effort));
                    } else {
                        *value = serde_json::json!(effort);
                    }
                }
                if provider_type == ProviderType::Anthropic
                    && configured_params.pointer("/thinking/type").and_then(serde_json::Value::as_str) == Some("disabled")
                {
                    configured_params["thinking"] = serde_json::json!({"type":"adaptive", "display":"summarized"});
                }
            }
            if let (Some(params), Some(configured)) = (
                routed.request.model.extra_params.as_object_mut(),
                configured_params.as_object(),
            ) {
                for (key, value) in configured {
                    params.entry(key.clone()).or_insert_with(|| value.clone());
                }
            }
            routed.request.model.model_id = model.model_id.clone();
            let recorder = start_recorder(&store, &routed, &model.model_hash, &model.display_name, provider_type, &request_url, &model.model_id).await?;
            let guard = recorder.cancel_on_drop();
            let config = ProviderConfig {
                kind: provider_kind(provider_type),
                request_url,
                api_key: model.api_key.clone(),
                custom_headers: if model.custom_headers_enabled {
                    custom_headers(
                        &model.custom_headers,
                        &invocation.conversation_id,
                    )?
                } else {
                    reqwest::header::HeaderMap::new()
                },
                max_output_tokens: model.max_output_tokens(),
                request_timeout,
                allowed_body_fields: None,
            };
            let client = clients.provider_client(request_timeout).await?;
            let provider = build_observed(&config, recorder.clone(), client)?;
            let _cancel_on_drop = guard;
            let mut stream = provider.stream(routed, cancellation.clone());

            let stream_started = std::time::Instant::now();
            tracing::debug!(
                model = %selected,
                request_timeout_ms = request_timeout.as_millis() as u64,
                stream_idle_timeout_ms = stream_idle_timeout.as_millis() as u64,
                "provider stream created"
            );
            let mut last_event_time = std::time::Instant::now();
            let mut event_count: u64 = 0;
            loop {
                let event = match next_provider_event(&mut stream, stream_idle_timeout).await {
                    Ok(Some(event)) => event,
                    Ok(None) => break,
                    Err(_) => {
                        let elapsed_ms = stream_started.elapsed().as_millis() as u64;
                        let error = stream_idle_timeout_error(stream_idle_timeout);
                        tracing::warn!(
                            error = %error,
                            elapsed_ms,
                            event_count,
                            idle_timeout_ms = stream_idle_timeout.as_millis() as u64,
                            "provider stream idle timeout"
                        );
                        Err(error)
                    }
                };
                let now = std::time::Instant::now();
                let gap_ms = now.duration_since(last_event_time).as_millis() as u64;
                let elapsed_ms = now.duration_since(stream_started).as_millis() as u64;
                event_count += 1;
                match event {
                    Ok(event) => {
                        if gap_ms > 5000 {
                            tracing::debug!(
                                gap_ms,
                                elapsed_ms,
                                event = event_name(&event),
                                event_count,
                                "slow gap detected between provider events"
                            );
                        }
                        recorder.event(&event).await?;
                        last_event_time = now;
                        yield event;
                    }
                    Err(error) => {
                        let error = normalize_provider_stream_error(error, request_timeout, now.duration_since(stream_started));
                        tracing::debug!(
                            error = %error,
                            elapsed_ms,
                            gap_ms,
                            event_count,
                            "provider stream error"
                        );
                        recorder.failed(&error).await?;
                        Err(error)?;
                    }
                }
            }
            finish_stream(&recorder, &cancellation).await?;
        })
    }
}

fn event_name(event: &super::ModelEvent) -> &'static str {
    match event {
        super::ModelEvent::Start { .. } => "Start",
        super::ModelEvent::TextStart => "TextStart",
        super::ModelEvent::TextDelta(_) => "TextDelta",
        super::ModelEvent::TextEnd => "TextEnd",
        super::ModelEvent::ThinkingStart => "ThinkingStart",
        super::ModelEvent::ThinkingDelta(_) => "ThinkingDelta",
        super::ModelEvent::ThinkingEnd => "ThinkingEnd",
        super::ModelEvent::ToolCallStart { .. } => "ToolCallStart",
        super::ModelEvent::ToolCallArgumentsDelta { .. } => "ToolCallArgsDelta",
        super::ModelEvent::ToolCallEnd { .. } => "ToolCallEnd",
        super::ModelEvent::ReasoningReplayState(_) => "ReasoningReplayState",
        super::ModelEvent::ProviderReplayState(_) => "ReplayState",
        super::ModelEvent::Usage(_) => "Usage",
        super::ModelEvent::Done(_) => "Done",
    }
}

async fn start_recorder(
    store: &Store,
    invocation: &ModelInvocation,
    model_hash: &str,
    display_name: &str,
    provider_type: ProviderType,
    request_url: &str,
    model_id: &str,
) -> Result<CallRecorder> {
    CallRecorder::start(
        store.clone(),
        NewLlmCall {
            call_id: invocation.call_id.clone(),
            run_id: invocation.run_id.clone(),
            conversation_id: invocation.conversation_id.clone(),
            provider_call_index: invocation.provider_call_index.min(i64::MAX as u64) as i64,
            model_hash: model_hash.into(),
            provider_type,
            provider_url: request_url.into(),
            request_type: provider_type,
            request_url: request_url.into(),
            model_id: model_id.into(),
            display_name: display_name.into(),
            reasoning_effort: invocation.request.model.reasoning.effort.clone(),
            fast: invocation.request.model.latency == ModelLatency::Fast,
            message_count: invocation.request.history.len(),
            tool_count: invocation.request.prompt.tools.len(),
            detailed: false,
        },
    )
    .await
}

async fn finish_stream(recorder: &CallRecorder, cancellation: &CancellationToken) -> Result<()> {
    if recorder.is_finished() {
        return Ok(());
    }
    if cancellation.is_cancelled() {
        recorder.cancelled().await
    } else {
        let error = Error::Provider("provider stream ended without Done".into());
        recorder.failed(&error).await?;
        Err(error)
    }
}

fn provider_kind(provider_type: ProviderType) -> ProviderKind {
    match provider_type {
        ProviderType::OpenAiChat => ProviderKind::OpenAiChat,
        ProviderType::OpenAiResponses => ProviderKind::OpenAiResponses,
        ProviderType::Anthropic => ProviderKind::Anthropic,
        // Plugin is retained only for reading historical call records.
        ProviderType::Plugin | ProviderType::OpenAiImages => unreachable!("not a chat provider"),
    }
}

async fn next_provider_event(
    stream: &mut ProviderStream,
    idle_timeout: Duration,
) -> std::result::Result<Option<Result<super::ModelEvent>>, tokio::time::error::Elapsed> {
    tokio::time::timeout(idle_timeout, stream.next()).await
}

fn stream_idle_timeout_error(idle_timeout: Duration) -> Error {
    Error::Provider(format!(
        "provider stream idle timeout: no events received for {} seconds ({} minutes)",
        idle_timeout.as_secs(),
        idle_timeout.as_secs() / 60
    ))
}

fn normalize_provider_stream_error(
    error: Error,
    request_timeout: Duration,
    elapsed: Duration,
) -> Error {
    match error {
        Error::Http(source) => {
            let source = source.without_url();
            if source.is_timeout() {
                let phase = if source.is_connect() {
                    "connection"
                } else if source.is_body() || source.is_decode() {
                    "response body read"
                } else {
                    "request"
                };
                Error::Provider(format!(
                    "provider {phase} timed out after {} ms (request timeout limit: {} ms): {}",
                    elapsed.as_millis(),
                    request_timeout.as_millis(),
                    root_error_message(&source)
                ))
            } else if source.is_body() || source.is_decode() {
                Error::Provider(format!(
                    "provider stream transport failed while reading the response body: {}",
                    root_error_message(&source)
                ))
            } else {
                Error::Http(source)
            }
        }
        error => error,
    }
}

fn root_error_message(error: &(dyn std::error::Error + 'static)) -> String {
    let mut current = error;
    while let Some(source) = current.source() {
        current = source;
    }
    current.to_string()
}

pub(crate) fn custom_headers(
    value: &serde_json::Value,
    conversation_id: &str,
) -> Result<reqwest::header::HeaderMap> {
    let rendered = super::request_template::render_json_strings(value, conversation_id);
    let object = rendered
        .as_object()
        .ok_or_else(|| Error::Config("custom headers must be an object".into()))?;
    let mut headers = reqwest::header::HeaderMap::new();
    for (name, value) in object {
        let name = reqwest::header::HeaderName::from_bytes(name.as_bytes())
            .map_err(|error| Error::Config(format!("invalid custom header name: {error}")))?;
        let value = value
            .as_str()
            .ok_or_else(|| Error::Config("custom header values must be strings".into()))?;
        let value = reqwest::header::HeaderValue::from_str(value)
            .map_err(|error| Error::Config(format!("invalid custom header value: {error}")))?;
        headers.insert(name, value);
    }
    Ok(headers)
}

pub fn build(config: &ProviderConfig) -> Result<Arc<dyn Provider>> {
    build_inner(config, None, None)
}

fn build_observed(
    config: &ProviderConfig,
    recorder: CallRecorder,
    client: reqwest::Client,
) -> Result<Arc<dyn Provider>> {
    build_inner(config, Some(recorder), Some(client))
}

fn build_inner(
    config: &ProviderConfig,
    recorder: Option<CallRecorder>,
    client: Option<reqwest::Client>,
) -> Result<Arc<dyn Provider>> {
    let client = match client {
        Some(client) => client,
        None => reqwest::Client::builder()
            .timeout(config.request_timeout)
            .build()?,
    };
    let provider: Arc<dyn Provider> = match config.kind {
        ProviderKind::OpenAiChat => {
            Arc::new(OpenAiChatProvider::new(client, config.clone()).with_recorder(recorder))
        }
        ProviderKind::OpenAiResponses => {
            Arc::new(OpenAiResponsesProvider::new(client, config.clone()).with_recorder(recorder))
        }
        ProviderKind::Anthropic => {
            Arc::new(AnthropicProvider::new(client, config.clone()).with_recorder(recorder))
        }
    };
    Ok(Arc::new(NormalizedProvider::new(provider)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_cursor_conversation_id_in_custom_header_values() {
        let template = serde_json::json!({
            "x-opencode-session-id": "{{SessionId}}",
            "x-label": "cursor/{{SessionId}}"
        });

        let headers = custom_headers(&template, "cursor-conversation-id").unwrap();

        assert_eq!(
            headers.get("x-opencode-session-id").unwrap(),
            "cursor-conversation-id"
        );
        assert_eq!(
            headers.get("x-label").unwrap(),
            "cursor/cursor-conversation-id"
        );
        assert_eq!(template["x-opencode-session-id"], "{{SessionId}}");
    }

    #[tokio::test]
    async fn pending_provider_event_hits_the_idle_timeout() {
        let mut stream: ProviderStream = Box::pin(futures_util::stream::pending());

        let result = next_provider_event(&mut stream, Duration::from_millis(1)).await;

        assert!(result.is_err());
    }

    async fn transport_server(
        response: &'static [u8],
        hold_open: bool,
    ) -> (String, tokio::task::JoinHandle<()>) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let task = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = [0_u8; 4096];
            socket.read(&mut request).await.unwrap();
            socket.write_all(response).await.unwrap();
            if hold_open {
                std::future::pending::<()>().await;
            }
        });
        (format!("http://{address}/?key=synthetic-secret"), task)
    }

    #[tokio::test]
    async fn connection_timeout_does_not_claim_the_total_request_limit_elapsed() {
        let (url, server) = transport_server(b"", true).await;
        let client = reqwest::Client::builder()
            .use_native_tls()
            .no_proxy()
            .connect_timeout(Duration::from_millis(50))
            .timeout(Duration::from_secs(3600))
            .build()
            .unwrap();
        let started = std::time::Instant::now();
        let error = client
            .get(url.replacen("http:", "https:", 1))
            .send()
            .await
            .unwrap_err();
        let elapsed = started.elapsed();
        server.abort();
        assert!(error.is_timeout() && error.is_connect());
        let message =
            normalize_provider_stream_error(Error::Http(error), Duration::from_secs(3600), elapsed)
                .to_string();
        assert!(message.contains("connection timed out"), "{message}");
        assert!(
            message.contains(&format!("after {} ms", elapsed.as_millis())),
            "{message}"
        );
        assert!(!message.contains("timed out after 3600"), "{message}");
        assert!(!message.contains("synthetic-secret"), "{message}");
    }

    #[tokio::test]
    async fn request_and_body_timeouts_report_the_actual_wait() {
        for (response, limit, phase) in [
            (b"".as_slice(), Duration::from_secs(3600), "request"),
            (
                b"HTTP/1.1 200 OK\r\nContent-Length: 100\r\n\r\n".as_slice(),
                Duration::from_secs(3600),
                "response body read",
            ),
            (b"".as_slice(), Duration::from_millis(50), "request"),
        ] {
            let (url, server) = transport_server(response, true).await;
            let client = reqwest::Client::builder()
                .no_proxy()
                .read_timeout(if limit < Duration::from_secs(1) {
                    Duration::from_secs(5)
                } else {
                    Duration::from_millis(50)
                })
                .timeout(limit)
                .build()
                .unwrap();
            let started = std::time::Instant::now();
            let error = match client.get(&url).send().await {
                Ok(response) => response.bytes().await.unwrap_err(),
                Err(error) => error,
            };
            let elapsed = started.elapsed();
            server.abort();
            assert!(error.is_timeout());
            let message =
                normalize_provider_stream_error(Error::Http(error), limit, elapsed).to_string();
            assert!(!message.contains("timed out after 3600"), "{message}");
            assert!(
                message.contains(&format!(
                    "provider {phase} timed out after {} ms",
                    elapsed.as_millis()
                )),
                "{message}"
            );
            assert!(
                message.contains(&format!("request timeout limit: {} ms", limit.as_millis())),
                "{message}"
            );
            assert!(!message.contains("synthetic-secret"), "{message}");
        }
    }

    #[tokio::test]
    async fn truncated_and_invalid_compressed_bodies_keep_the_transport_cause() {
        for response in [
            b"HTTP/1.1 200 OK\r\nContent-Length: 100\r\n\r\nshort".as_slice(),
            b"HTTP/1.1 200 OK\r\nContent-Encoding: gzip\r\nContent-Length: 8\r\n\r\nnot-gzip"
                .as_slice(),
        ] {
            let (url, server) = transport_server(response, false).await;
            let error = reqwest::Client::builder()
                .no_proxy()
                .build()
                .unwrap()
                .get(&url)
                .send()
                .await
                .unwrap()
                .bytes()
                .await
                .unwrap_err();
            server.await.unwrap();
            assert!(error.is_body() || error.is_decode());
            let cause = root_error_message(&error);
            let message = normalize_provider_stream_error(
                Error::Http(error),
                Duration::from_secs(3600),
                Duration::ZERO,
            )
            .to_string();
            assert!(
                message.contains("while reading the response body"),
                "{message}"
            );
            assert!(message.contains(&cause), "{message}");
            assert!(!message.contains("synthetic-secret"), "{message}");
        }
    }

    #[tokio::test]
    async fn other_http_errors_do_not_expose_the_request_url() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        drop(listener);
        let error = reqwest::Client::builder()
            .no_proxy()
            .build()
            .unwrap()
            .get(format!("http://{address}/?key=synthetic-secret"))
            .send()
            .await
            .unwrap_err();
        assert!(error.is_connect() && !error.is_timeout());
        let error = normalize_provider_stream_error(
            Error::Http(error),
            Duration::from_secs(3600),
            Duration::ZERO,
        );
        assert!(
            matches!(&error, Error::Http(source) if source.is_connect() && source.url().is_none())
        );
        assert!(!error.to_string().contains("synthetic-secret"));
    }

    #[test]
    fn idle_timeout_states_the_boundary_and_duration() {
        let Error::Provider(idle) = stream_idle_timeout_error(Duration::from_secs(30 * 60)) else {
            panic!("idle timeout must be a provider error");
        };
        assert_eq!(
            idle,
            "provider stream idle timeout: no events received for 1800 seconds (30 minutes)"
        );
    }
}
