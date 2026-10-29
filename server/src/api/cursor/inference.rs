//! HTTP/1 inference sessions. Cursor owns the Agent loop; this service owns model calls.
use std::{
    collections::{BTreeMap, HashMap},
    sync::{
        atomic::{AtomicU8, Ordering},
        Arc,
    },
    time::Duration,
};

use axum::{
    body::{to_bytes, Body},
    extract::Extension,
    http::{header, Request, Response},
};
use bytes::Bytes;
use futures_util::StreamExt;
use prost::Message;
use tokio::sync::{mpsc, Mutex, Notify};
use tokio_util::sync::CancellationToken;

use super::proxy::{self, CursorProxy};
use crate::{
    cursor::{
        inference::{invocation, InvocationOutput},
        protocol::{
            connect,
            proto::{agent::v1 as agent, aiserver::v1 as ai, inference as pb},
        },
        transport::{OutputHub, TransportRegistry},
    },
    provider::Provider,
    store::Store,
    Error, Result,
};

#[derive(Clone)]
pub struct InferenceService {
    inner: Arc<ServiceInner>,
}
struct ServiceInner {
    sessions: Mutex<HashMap<String, Arc<Session>>>,
    store: Store,
    provider: Arc<dyn Provider>,
}
struct Session {
    commands: mpsc::Sender<(i64, pb::RunInferenceClientMessage)>,
    output: Arc<OutputHub>,
    cancellation: CancellationToken,
    route: AtomicU8,
    route_changed: Notify,
}

impl InferenceService {
    pub fn new(registry: &TransportRegistry) -> Self {
        Self {
            inner: Arc::new(ServiceInner {
                sessions: Mutex::new(HashMap::new()),
                store: registry.store().clone(),
                provider: registry.conversations().dependencies().provider.clone(),
            }),
        }
    }

    async fn session(&self, id: &str) -> Arc<Session> {
        let mut sessions = self.inner.sessions.lock().await;
        if let Some(session) = sessions.get(id) {
            return session.clone();
        }
        let (commands, receiver) = mpsc::channel(128);
        let session = Arc::new(Session {
            commands,
            output: Arc::default(),
            cancellation: CancellationToken::new(),
            route: AtomicU8::new(0),
            route_changed: Notify::new(),
        });
        sessions.insert(id.into(), session.clone());
        let service = self.clone();
        let request_id = id.to_owned();
        let actor = session.clone();
        tokio::spawn(async move {
            if let Err(error) = service.run(&request_id, &actor, receiver).await {
                tracing::warn!(%request_id, %error, "RunInference failed");
                if let Ok(frame) = connect::encode_error_end_stream(&connect::ConnectStreamError {
                    code: connect::ConnectCode::Unavailable,
                    message: error.to_string(),
                    details: Vec::new(),
                }) {
                    actor.output.emit(frame);
                }
            } else {
                actor.output.emit(connect::encode_end_stream());
            }
            actor.output.close();
            actor.cancellation.cancel();
            service.inner.sessions.lock().await.remove(&request_id);
        });
        session
    }

    /// The shared Bidi endpoint carries messages for distinct RPC services.
    /// Only claim a new session when its first frame has the inference run schema.
    pub async fn append(&self, request: &ai::BidiAppendRequest) -> Result<Option<bool>> {
        let id = request
            .request_id
            .as_ref()
            .map(|id| id.request_id.as_str())
            .filter(|id| !id.is_empty())
            .ok_or_else(|| Error::Protocol("Bidi request ID is required".into()))?;
        let known = self.inner.sessions.lock().await.get(id).cloned();
        let payload = if request.data_binary.is_empty() {
            hex::decode(&request.data)
                .map_err(|e| Error::Protocol(format!("invalid Bidi hex: {e}")))?
        } else {
            request.data_binary.clone()
        };
        let decoded = pb::RunInferenceClientMessage::decode(payload.as_slice());
        if known.is_none()
            && !matches!(&decoded, Ok(pb::RunInferenceClientMessage {
            message: Some(pb::run_inference_client_message::Message::RunRequest(r))
        }) if !r.conversation_id.is_empty() && r.requested_model.is_some())
        {
            return Ok(None);
        }
        let message = decoded?;
        let session = match known {
            Some(session) => session,
            None => self.session(id).await,
        };
        session
            .commands
            .send((request.append_seqno, message))
            .await
            .map_err(|_| Error::Protocol("inference session is closed".into()))?;
        Ok(Some(wait_route(&session).await?))
    }

    async fn run(
        &self,
        request_id: &str,
        session: &Arc<Session>,
        mut receiver: mpsc::Receiver<(i64, pb::RunInferenceClientMessage)>,
    ) -> Result<()> {
        use pb::run_inference_client_message::Message as M;
        let mut pending = BTreeMap::new();
        let mut next = 0;
        let mut selected = None;
        let mut calls = HashMap::<String, CancellationToken>::new();
        let mut call_index = 0;
        let mut heartbeat = tokio::time::interval(Duration::from_secs(5));
        heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let idle = tokio::time::sleep(Duration::from_secs(1800));
        tokio::pin!(idle);
        loop {
            let command = tokio::select! {
                _ = session.cancellation.cancelled() => return Ok(()),
                _ = heartbeat.tick() => {
                    if session.route.load(Ordering::Acquire) == 1 {
                        emit(&session.output, pb::run_inference_server_message::Message::Heartbeat(pb::RunInferenceHeartbeat::default()))?;
                    }
                    continue;
                }
                _ = &mut idle => return Err(Error::Protocol("inference session expired".into())),
                command = receiver.recv() => command,
            };
            idle.as_mut()
                .reset(tokio::time::Instant::now() + Duration::from_secs(1800));
            let Some((seq, message)) = command else {
                return Ok(());
            };
            if seq < next {
                continue;
            }
            if pending.len() >= 128 {
                return Err(Error::Protocol(
                    "inference append ordering window exceeded".into(),
                ));
            }
            pending.entry(seq).or_insert(message);
            while let Some(message) = pending.remove(&next) {
                next += 1;
                if session.route.load(Ordering::Acquire) == 2 {
                    continue;
                }
                match message.message {
                    Some(M::RunRequest(request)) => {
                        if selected.is_some() {
                            return Err(Error::Protocol("duplicate inference run request".into()));
                        }
                        let model = request
                            .requested_model
                            .as_ref()
                            .ok_or_else(|| Error::Protocol("inference model is required".into()))?;
                        let configured = self.inner.store.model(&model.model_id).await?;
                        let local = configured.is_some()
                            || model.model_id.starts_with(crate::plugin::ADAPTER_ID_PREFIX);
                        session
                            .route
                            .store(if local { 1 } else { 2 }, Ordering::Release);
                        session.route_changed.notify_waiters();
                        if !local {
                            continue;
                        }
                        let mut ready = pb::RunInferenceRunReady {
                            resolved_model: Some(model.clone()),
                            accepts_client_heartbeat: true,
                            accepts_agent_events: true,
                            accepts_aborted_finish: true,
                            accepts_subagent_model_resolution: true,
                            ..Default::default()
                        };
                        ready.prompt_model_metadata =
                            Some(crate::cursor::inference::prompt_metadata(
                                &request,
                                configured.as_ref(),
                                &self.inner.store.models().await?,
                            ));
                        ready.routed_model_display_name =
                            configured.as_ref().map(|m| m.display_name.clone());
                        tracing::info!(%request_id, conversation_id=%request.conversation_id, model_id=%model.model_id, "routing RunInference to BYOK provider");
                        emit(
                            &session.output,
                            pb::run_inference_server_message::Message::RunReady(ready),
                        )?;
                        selected = Some(request);
                    }
                    Some(M::InvokeModel(invoke)) => {
                        let run = selected.as_ref().ok_or_else(|| {
                            Error::Protocol("inference run must start before invocation".into())
                        })?;
                        let id = invoke.invocation_id.clone();
                        if id.is_empty() || calls.contains_key(&id) {
                            return Err(Error::Protocol(
                                "invalid or duplicate invocation ID".into(),
                            ));
                        }
                        if invoke.delta_base.is_some() || invoke.compressed_request.is_some() {
                            // These capabilities are not advertised in run_ready.
                            end_invocation(
                                &session.output,
                                &id,
                                Some(Error::Protocol(
                                    "unnegotiated delta/compressed inference request".into(),
                                )),
                            )?;
                            continue;
                        }
                        let request = invoke.request.ok_or_else(|| {
                            Error::Protocol("inference invocation request is missing".into())
                        })?;
                        let model = run
                            .requested_model
                            .as_ref()
                            .expect("validated run model")
                            .clone();
                        let cancellation = session.cancellation.child_token();
                        calls.insert(id.clone(), cancellation.clone());
                        let provider = self.inner.provider.clone();
                        let output = session.output.clone();
                        let conversation = run.conversation_id.clone();
                        let run_id = request_id.to_owned();
                        let index = call_index;
                        call_index += 1;
                        tokio::spawn(async move {
                            let result = stream_invocation(
                                provider,
                                &output,
                                &request,
                                &model,
                                &conversation,
                                &run_id,
                                &id,
                                index,
                                cancellation,
                            )
                            .await;
                            if let Err(error) = &result {
                                tracing::warn!(%run_id, invocation_id=%id, %error, "inference provider invocation failed");
                            }
                            let _ = end_invocation(&output, &id, result.err());
                        });
                    }
                    Some(M::CancelInvocation(cancel)) => {
                        if let Some(token) = calls.get(&cancel.invocation_id) {
                            token.cancel();
                        }
                    }
                    Some(M::FinishRun(_)) => return Ok(()),
                    Some(M::ResolveSubagentModels(request)) => {
                        let mut models = Vec::new();
                        for requested in request.requested_models {
                            let valid = self.inner.store.model(&requested).await?.is_some()
                                || requested.starts_with(crate::plugin::ADAPTER_ID_PREFIX);
                            models.push(pb::RunInferenceResolvedSubagentModel {
                                requested: requested.clone(),
                                resolved_slug: valid.then(|| requested.clone()),
                                policy: Some(pb::RunInferenceSubagentModelPolicy {
                                    slug: requested,
                                    valid,
                                    advertised: valid,
                                    blocked: !valid,
                                    ..Default::default()
                                }),
                            });
                        }
                        emit(
                            &session.output,
                            pb::run_inference_server_message::Message::SubagentModelsResolved(
                                pb::RunInferenceSubagentModelsResolved {
                                    resolution_id: request.resolution_id,
                                    models,
                                },
                            ),
                        )?;
                    }
                    Some(M::AgentEvent(event)) => {
                        tracing::debug!(%request_id, method = %event.method_name, "received Cursor agent event");
                    }
                    Some(
                        M::ClientHeartbeat(_) | M::RunRequestLog(_) | M::ClientFirstCallTimings(_),
                    ) => {}
                    Some(M::DeltaBaseSeed(_)) => {
                        return Err(Error::Protocol(
                            "delta base seeds were not negotiated".into(),
                        ))
                    }
                    None => return Err(Error::Protocol("empty inference message".into())),
                }
            }
        }
    }
}

async fn wait_route(session: &Session) -> Result<bool> {
    tokio::time::timeout(Duration::from_secs(60), async {
        loop {
            let changed = session.route_changed.notified();
            match session.route.load(Ordering::Acquire) { 1 => return Ok(true), 2 => return Ok(false), _ => {} }
            tokio::select! { _ = changed => {}, _ = session.cancellation.cancelled() => return Err(Error::Protocol("inference closed before model selection".into())) }
        }
    }).await.map_err(|_| Error::Protocol("timed out waiting for inference model selection".into()))?
}

fn emit(output: &OutputHub, message: pb::run_inference_server_message::Message) -> Result<()> {
    output.emit(connect::encode_message(&pb::RunInferenceServerMessage {
        message: Some(message),
    })?);
    Ok(())
}
fn end_invocation(output: &OutputHub, id: &str, error: Option<Error>) -> Result<()> {
    emit(
        output,
        pb::run_inference_server_message::Message::InvocationEnd(pb::RunInferenceInvocationEnd {
            invocation_id: id.into(),
            error: error.map(|error| pb::RunInferenceInvocationError {
                code: match &error {
                    Error::Cancelled => 1,
                    Error::Protocol(_) | Error::Decode(_) | Error::Json(_) => 3,
                    _ => 13,
                },
                message: error.to_string(),
                ..Default::default()
            }),
            ..Default::default()
        }),
    )
}

#[allow(clippy::too_many_arguments)]
async fn stream_invocation(
    provider: Arc<dyn Provider>,
    output: &OutputHub,
    request: &pb::InferenceStreamRequest,
    model: &pb::InferenceRequestedModel,
    conversation: &str,
    run: &str,
    id: &str,
    index: u64,
    cancellation: CancellationToken,
) -> Result<()> {
    let mut request = request.clone();
    let selected = request.requested_model.get_or_insert_with(|| model.clone());
    let model_id = selected.model_id.clone();
    let input = invocation(&request, &model_id, conversation, run, id, index)?;
    let mut stream = provider.stream(input, cancellation.clone());
    let mut mapped = InvocationOutput::default();
    emit(
        output,
        pb::run_inference_server_message::Message::InvocationResponse(
            pb::RunInferenceInvocationResponse {
                invocation_id: id.into(),
                response: Some(pb::InferenceStreamResponse {
                    response: Some(pb::inference_stream_response::Response::InvocationId(
                        pb::InferenceInvocationIdInfo {
                            invocation_id: id.into(),
                        },
                    )),
                    ..Default::default()
                }),
            },
        ),
    )?;
    loop {
        let event = tokio::select! { _ = cancellation.cancelled() => return Err(Error::Cancelled), event = stream.next() => event };
        let Some(event) = event else {
            break;
        };
        for response in mapped.event(event?, &model_id)? {
            emit(
                output,
                pb::run_inference_server_message::Message::InvocationResponse(
                    pb::RunInferenceInvocationResponse {
                        invocation_id: id.into(),
                        response: Some(response),
                    },
                ),
            )?;
        }
        if mapped.done {
            return Ok(());
        }
    }
    Err(Error::Provider(
        "inference provider ended without completion".into(),
    ))
}

struct StreamGuard {
    session: Arc<Session>,
}
impl Drop for StreamGuard {
    fn drop(&mut self) {
        self.session.cancellation.cancel();
    }
}

pub async fn stream(
    Extension(service): Extension<InferenceService>,
    Extension(proxy): Extension<CursorProxy>,
    request: Request<Body>,
) -> Result<Response<Body>> {
    let (parts, body) = request.into_parts();
    let body = to_bytes(body, 1024 * 1024)
        .await
        .map_err(|e| Error::Protocol(e.to_string()))?;
    let id: agent::BidiRequestId = connect::decode_unary(&body)?;
    if id.request_id.is_empty() {
        return Err(Error::Protocol("inference request ID is required".into()));
    }
    let session = service.session(&id.request_id).await;
    if !wait_route(&session).await? {
        return proxy::forward(
            Extension(proxy),
            Request::from_parts(parts, Body::from(body)),
        )
        .await;
    }
    let mut receiver = session.output.subscribe();
    let guard = StreamGuard { session };
    let stream = async_stream::stream! {
        let _guard = guard;
        while let Some(frame) = receiver.recv().await { yield Ok::<Bytes, std::convert::Infallible>(frame); }
    };
    Ok(Response::builder()
        .header(header::CONTENT_TYPE, "text/event-stream")
        .header(header::CACHE_CONTROL, "no-cache")
        .header("connect-protocol-version", "1")
        .body(Body::from_stream(stream))
        .expect("static inference response headers"))
}
