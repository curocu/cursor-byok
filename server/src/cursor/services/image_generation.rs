//! Adapts the native image RPC; Cursor owns tool lifecycle and file writes.
use axum::{
    body::{to_bytes, Body},
    extract::{Extension, State},
    http::{header, Request, Response},
};
use prost::Message;
use tokio_util::sync::CancellationToken;

use crate::{
    api::cursor::proxy::{self, CursorProxy},
    cursor::{
        protocol::{connect, proto::aiserver::v1 as ai},
        transport::TransportRegistry,
    },
    provider::image_generation::{generate, IMAGE_TIMEOUT},
    Result,
};

pub async fn run(
    State(registry): State<TransportRegistry>,
    Extension(proxy): Extension<CursorProxy>,
    request: Request<Body>,
) -> Result<Response<Body>> {
    let settings = registry.store().image_generation_settings().await?;
    if settings.model_hash.is_empty() {
        return proxy::forward(Extension(proxy), request).await;
    }
    let timeout = request
        .headers()
        .get("connect-timeout-ms")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<u64>().ok())
        .map(std::time::Duration::from_millis)
        .unwrap_or(IMAGE_TIMEOUT);
    let bytes = to_bytes(request.into_body(), 32 * 1024 * 1024)
        .await
        .map_err(|_| {
            crate::Error::Protocol("image request exceeds body limit or cannot be read".into())
        })?;
    let request: ai::RunGenerateImageRequest = connect::decode_unary(&bytes)?;
    let store = registry.store().clone();
    // Closing the native RPC drops this body and the outbound future. No detached job survives.
    let stream = async_stream::stream! {
        let id = uuid::Uuid::new_v4();
        let started = std::time::Instant::now();
        tracing::info!(image_request_id=%id, model_hash=%settings.model_hash, "native image generation started");
        let result = match store.model(&settings.model_hash).await {
            Ok(Some(model)) => generate(&store, &model, &request, &CancellationToken::new(), timeout).await,
            _ => Err(crate::provider::image_generation::ImageGenerationError {
                message: "configured default image model is unavailable; select an image model in BYOK".into(),
                provider_status_code: None, content_safety_blocked: false,
            }),
        };
        let result = match result {
            Ok(image) => {
                tracing::info!(image_request_id=%id, duration_ms=started.elapsed().as_millis() as u64, width=image.width, height=image.height, "native image generation completed");
                ai::run_generate_image_response::Result::Success(ai::RunGenerateImageSuccess { image_data: image.image_data, mime_type: image.mime_type })
            },
            Err(error) => {
                tracing::warn!(image_request_id=%id, duration_ms=started.elapsed().as_millis() as u64, provider_status_code=?error.provider_status_code, safety_blocked=error.content_safety_blocked, "native image generation failed");
                ai::run_generate_image_response::Result::Error(ai::RunGenerateImageError {
                    error: error.message, provider_status_code: error.provider_status_code,
                    content_safety_blocked: error.content_safety_blocked, model_restricted: false,
                })
            },
        };
        yield Ok::<_, std::io::Error>(ai::RunGenerateImageResponse { result: Some(result) }.encode_to_vec());
    };
    Ok(Response::builder()
        .header(header::CONTENT_TYPE, "application/proto")
        .body(Body::from_stream(stream))
        .expect("static image response"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        config::{ProviderConfig, ProviderKind},
        cursor::prompting::{PromptAssets, PromptCompiler},
        network::NetworkClients,
        provider::build_provider,
        store::Store,
    };
    use axum::{routing::post, Router};
    use std::{sync::Arc, time::Duration};

    #[tokio::test]
    async fn disabled_image_generation_forwards_unknown_fields_and_headers_unchanged() {
        let bytes = vec![10, 4, b'd', b'r', b'a', b'w', 160, 6, 7];
        let expected = bytes.clone();
        let upstream = Router::new().route(
            "/native-image",
            post(move |request: Request<Body>| {
                let expected = expected.clone();
                async move {
                    assert_eq!(request.headers()["x-test-native"], "keep");
                    assert_eq!(
                        to_bytes(request.into_body(), 4096).await.unwrap().as_ref(),
                        expected
                    );
                    "official response unchanged"
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let task = tokio::spawn(async move { axum::serve(listener, upstream).await.unwrap() });
        let dir = tempfile::tempdir().unwrap();
        let store = Store::connect(&format!(
            "sqlite://{}",
            dir.path().join("image.db").display()
        ))
        .await
        .unwrap();
        let provider = build_provider(&ProviderConfig {
            kind: ProviderKind::OpenAiChat,
            request_url: "http://localhost".into(),
            api_key: String::new(),
            custom_headers: Default::default(),
            max_output_tokens: None,
            request_timeout: Duration::from_secs(1),
            allowed_body_fields: None,
        })
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
        let response = run(
            State(registry),
            Extension(CursorProxy::for_test(NetworkClients::new(store), url)),
            Request::post("/native-image")
                .header("x-test-native", "keep")
                .body(Body::from(bytes))
                .unwrap(),
        )
        .await
        .unwrap();
        assert_eq!(
            to_bytes(response.into_body(), 4096).await.unwrap(),
            "official response unchanged"
        );
        task.abort();
    }
}
