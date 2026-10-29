//! Acknowledges optional official labeling records for locally configured models.
use axum::{
    body::{to_bytes, Body},
    extract::{Extension, State},
    http::{header, Request, Response},
};
use prost::Message;

use crate::{
    api::cursor::proxy::{self, CursorProxy},
    cursor::{protocol::connect, transport::TransportRegistry},
    local_app, Result,
};

pub const FOLLOWUP_PATH: &str = "/aiserver.v1.InferenceService/RecordAgentFollowupClassification";
pub const POST_TURN_PATH: &str = "/aiserver.v1.InferenceService/RecordAgentPostTurnLabeling";

// Only model selection is needed; prost skips the conversation-content fields.
// Tags come from Cursor 3.27.22's InferenceService descriptors.
#[derive(Clone, PartialEq, Message)]
struct FollowupRecord {
    #[prost(string, optional, tag = "6")]
    model_name: Option<String>,
}

#[derive(Clone, PartialEq, Message)]
struct PostTurnRecord {
    #[prost(string, optional, tag = "5")]
    model_name: Option<String>,
}

pub async fn record(
    State(registry): State<TransportRegistry>,
    Extension(proxy): Extension<CursorProxy>,
    request: Request<Body>,
) -> Result<Response<Body>> {
    let (parts, body) = request.into_parts();
    let body = to_bytes(body, usize::MAX)
        .await
        .map_err(|error| crate::Error::Protocol(format!("cannot read labeling record: {error}")))?;
    let model = if parts.uri.path() == FOLLOWUP_PATH {
        connect::decode_unary::<FollowupRecord>(&body)?.model_name
    } else {
        connect::decode_unary::<PostTurnRecord>(&body)?.model_name
    };
    let model = model.as_deref().unwrap_or_default();
    let local = local_app::request_uses_local_cursor_token(&parts.headers)
        || model.starts_with(crate::plugin::ADAPTER_ID_PREFIX)
        || registry.store().model(model).await?.is_some();
    if local {
        // Both official methods return google.protobuf.Empty. Local provider
        // calls are already recorded separately; no official categorizer is run.
        tracing::debug!(
            model_id = model,
            path = parts.uri.path(),
            "accepted local inference labeling record"
        );
        return Ok(Response::builder()
            .header(header::CONTENT_TYPE, "application/proto")
            .header(header::CONTENT_LENGTH, "0")
            .body(Body::empty())
            .expect("static labeling response headers"));
    }
    proxy::forward(
        Extension(proxy),
        Request::from_parts(parts, Body::from(body)),
    )
    .await
}
