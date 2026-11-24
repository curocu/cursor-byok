//! One native image-tool invocation; never falls back to a different provider.
use std::{io::Cursor, time::Duration};

use base64::{engine::general_purpose::STANDARD, Engine};
use futures_util::StreamExt;
use reqwest::multipart::{Form, Part};
use serde::Serialize;
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;

use crate::{
    cursor::protocol::proto::aiserver::v1 as ai,
    model::{ModelConfig, OPENAI_IMAGES_ENDPOINT},
    store::Store,
};

const MAX_IMAGE_BYTES: usize = 20 * 1024 * 1024;
const MAX_RESPONSE_BYTES: usize = 32 * 1024 * 1024;
const IMAGE_OPTION_KEYS: [&str; 7] = [
    "size",
    "quality",
    "output_format",
    "background",
    "output_compression",
    "moderation",
    "complexity",
];
pub const IMAGE_TIMEOUT: Duration = Duration::from_secs(300);

#[derive(Clone, Serialize)]
pub struct GeneratedImage {
    pub image_data: String,
    pub mime_type: String,
    pub width: u32,
    pub height: u32,
}

impl std::fmt::Debug for GeneratedImage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GeneratedImage")
            .field("mime_type", &self.mime_type)
            .field("width", &self.width)
            .field("height", &self.height)
            .finish_non_exhaustive()
    }
}

#[derive(Debug, thiserror::Error)]
#[error("{message}")]
pub struct ImageGenerationError {
    pub message: String,
    pub provider_status_code: Option<i32>,
    pub content_safety_blocked: bool,
}

impl ImageGenerationError {
    fn local(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            provider_status_code: None,
            content_safety_blocked: false,
        }
    }
}

pub async fn generate(
    store: &Store,
    model: &ModelConfig,
    request: &ai::RunGenerateImageRequest,
    cancellation: &CancellationToken,
    timeout: Duration,
) -> Result<GeneratedImage, ImageGenerationError> {
    tokio::select! {
        biased;
        _ = cancellation.cancelled() => Err(ImageGenerationError::local("image generation cancelled")),
        result = tokio::time::timeout(timeout.min(IMAGE_TIMEOUT), generate_inner(store, model, request)) => {
            result.unwrap_or_else(|_| Err(ImageGenerationError::local("image generation timed out")))
        }
    }
}

async fn generate_inner(
    store: &Store,
    model: &ModelConfig,
    request: &ai::RunGenerateImageRequest,
) -> Result<GeneratedImage, ImageGenerationError> {
    if !model.is_image_generation() {
        return Err(ImageGenerationError::local(
            "default image model does not use an image generation protocol",
        ));
    }
    if request.description.trim().is_empty() || request.description.len() > 100_000 {
        return Err(ImageGenerationError::local(
            "image description must contain 1–100000 bytes",
        ));
    }
    if request.reference_images.len() > 16 {
        return Err(ImageGenerationError::local(
            "at most 16 reference images are supported",
        ));
    }
    let mut references = Vec::new();
    let mut total = 0;
    for reference in &request.reference_images {
        let image = validate_image(&reference.data)?;
        if image.mime_type != reference.mime_type {
            return Err(ImageGenerationError::local(
                "reference image MIME type does not match its bytes",
            ));
        }
        let bytes = STANDARD
            .decode(&image.image_data)
            .expect("validated base64");
        total += bytes.len();
        if total > MAX_IMAGE_BYTES {
            return Err(ImageGenerationError::local(
                "reference images exceed 20 MiB in total",
            ));
        }
        references.push((image, bytes));
    }
    let client = crate::network::client_builder(store)
        .await
        .map_err(|_| ImageGenerationError::local("cannot configure image provider network client"))?
        .redirect(reqwest::redirect::Policy::none())
        .timeout(IMAGE_TIMEOUT)
        .build()
        .map_err(|_| ImageGenerationError::local("cannot build image provider network client"))?;
    let mut url = model
        .request_url()
        .map_err(|_| ImageGenerationError::local("invalid image provider request URL"))?;
    let images_api = model.openai_endpoint == OPENAI_IMAGES_ENDPOINT;
    let mut prompt = request.description.clone();
    let size = aspect_size(request.aspect_ratio.as_deref(), &mut prompt)?;
    let extra = model
        .extra_params()
        .as_object()
        .ok_or_else(|| ImageGenerationError::local("image extra params must be an object"))?;
    let mut body = Value::Object(extra.clone());
    let mut responses_options = serde_json::Map::new();
    if images_api {
        // A model switched from Responses may still carry a single image tool.
        // Images API has no tools, so lift known options to the request root.
        if let Some(tools) = body.get("tools").cloned() {
            let tool = match tools {
                Value::Array(tools)
                    if tools.len() == 1
                        && tools[0].get("type").and_then(Value::as_str)
                            == Some("image_generation") =>
                {
                    tools[0].clone()
                }
                _ => {
                    return Err(ImageGenerationError::local(
                        "Images API does not accept tools",
                    ))
                }
            };
            for key in IMAGE_OPTION_KEYS {
                if body.get(key).is_none() {
                    if let Some(value) = tool.get(key).cloned() {
                        body[key] = value;
                    }
                }
            }
            body.as_object_mut().expect("object").remove("tools");
        }
    } else {
        // A model switched from Images may still carry image options at the
        // request root. Responses only accepts them inside the image tool.
        for key in IMAGE_OPTION_KEYS {
            if let Some(value) = body.as_object_mut().expect("object").remove(key) {
                responses_options.insert(key.into(), value);
            }
        }
    }
    for key in [
        "model",
        "prompt",
        "input",
        "messages",
        "image",
        "images",
        "stream",
        "n",
        "tool_choice",
        "previous_response_id",
        "background",
    ] {
        // background is an image option for Images API; it is a job-lifecycle switch in Responses.
        if body.get(key).is_some() && !(images_api && key == "background") {
            return Err(ImageGenerationError::local(format!(
                "image extra params cannot replace {key}"
            )));
        }
    }
    body["model"] = json!(model.model_id);
    if images_api {
        body["prompt"] = json!(prompt);
        body["n"] = json!(1);
        apply_size(&mut body, request.aspect_ratio.as_deref(), size)?;
    } else {
        let mut content = vec![json!({"type":"input_text", "text":prompt})];
        for (image, _) in &references {
            content.push(json!({"type":"input_image", "image_url":format!("data:{};base64,{}", image.mime_type, image.image_data)}));
        }
        body["input"] = json!([{"role":"user", "content":content}]);
        body["stream"] = json!(false);
        body["tool_choice"] = json!({"type":"image_generation"});
        let mut tool = match body.get("tools") {
            Some(Value::Array(tools))
                if tools.len() == 1 && tools[0]["type"] == "image_generation" =>
            {
                tools[0].clone()
            }
            Some(_) => {
                return Err(ImageGenerationError::local(
                    "Responses image tools must contain exactly one image_generation tool",
                ))
            }
            None => json!({"type":"image_generation"}),
        };
        if let Some(object) = tool.as_object_mut() {
            for (key, value) in std::mem::take(&mut responses_options) {
                object.entry(key).or_insert(value);
            }
        }
        apply_size(&mut tool, request.aspect_ratio.as_deref(), size)?;
        body["tools"] = json!([tool]);
    }
    let mut upstream;
    if images_api && !references.is_empty() {
        // A complete custom URL must explicitly name the edits endpoint. Never guess a custom route.
        if !model.use_full_url {
            let mut parsed = reqwest::Url::parse(&url)
                .map_err(|_| ImageGenerationError::local("invalid image request URL"))?;
            let path = parsed
                .path()
                .strip_suffix("/generations")
                .ok_or_else(|| ImageGenerationError::local("cannot resolve image edits endpoint"))?
                .to_owned();
            parsed.set_path(&format!("{path}/edits"));
            url = parsed.into();
        } else if !reqwest::Url::parse(&url).is_ok_and(|url| url.path().ends_with("/images/edits"))
        {
            return Err(ImageGenerationError::local("reference images require a complete /images/edits URL, or disable full URL to resolve the standard endpoint"));
        }
        let mut form = Form::new();
        for (key, value) in body.as_object().expect("object") {
            let text = value
                .as_str()
                .map(str::to_owned)
                .unwrap_or_else(|| value.to_string());
            form = form.text(key.clone(), text);
        }
        for (index, (image, bytes)) in references.iter().enumerate() {
            let extension = image
                .mime_type
                .strip_prefix("image/")
                .expect("validated image MIME");
            let part = Part::bytes(bytes.clone())
                .file_name(format!("reference-{index}.{extension}"))
                .mime_str(&image.mime_type)
                .map_err(|_| ImageGenerationError::local("invalid reference MIME"))?;
            form = form.part("image[]", part);
        }
        upstream = client.post(&url).multipart(form);
    } else {
        upstream = client.post(&url).json(&body);
    }
    upstream = upstream.bearer_auth(&model.api_key);
    if model.custom_headers_enabled {
        let headers = super::router::custom_headers(&model.custom_headers, "")
            .map_err(|_| ImageGenerationError::local("invalid custom image headers"))?;
        if headers.contains_key(reqwest::header::CONTENT_TYPE)
            || headers.contains_key(reqwest::header::CONTENT_LENGTH)
        {
            return Err(ImageGenerationError::local(
                "image Content-Type and Content-Length are set by the selected protocol",
            ));
        }
        upstream = upstream.headers(headers);
    }
    let response = upstream.send().await.map_err(transport_error)?;
    let status = response.status();
    let retry_after = response
        .headers()
        .get("retry-after")
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);
    let bytes = limited_body(response, MAX_RESPONSE_BYTES).await?;
    let value: Value = serde_json::from_slice(&bytes)
        .or_else(|_| {
            if !status.is_success() {
                Ok(json!({"error":{"message":"provider returned a non-JSON error response"}}))
            } else {
                Err(())
            }
        })
        .map_err(|_| ImageGenerationError {
            message: format!(
                "image provider returned HTTP {} without a JSON image response",
                status.as_u16()
            ),
            provider_status_code: Some(i32::from(status.as_u16())),
            content_safety_blocked: false,
        })?;
    if !status.is_success() || value.get("error").is_some_and(|v| !v.is_null()) {
        return Err(provider_error(
            model,
            request,
            &value,
            status.as_u16(),
            retry_after.as_deref(),
        ));
    }
    if !images_api
        && value
            .get("status")
            .and_then(Value::as_str)
            .is_some_and(|s| s != "completed")
    {
        return Err(ImageGenerationError::local(
            "Responses image generation did not complete",
        ));
    }
    let image = if images_api {
        value
            .pointer("/data/0")
            .ok_or_else(|| ImageGenerationError::local("image provider returned no image"))?
    } else {
        value
            .get("output")
            .and_then(Value::as_array)
            .and_then(|items| {
                items.iter().find(|item| {
                    item["type"] == "image_generation_call" && item["status"] == "completed"
                })
            })
            .ok_or_else(|| {
                ImageGenerationError::local("Responses returned no completed image_generation_call")
            })?
    };
    if let Some(data) = image
        .get(if images_api { "b64_json" } else { "result" })
        .and_then(Value::as_str)
    {
        return validate_image(data);
    }
    if let Some(url) = image.get("url").and_then(Value::as_str) {
        let url = reqwest::Url::parse(url)
            .map_err(|_| ImageGenerationError::local("invalid generated image URL"))?;
        if !matches!(url.scheme(), "http" | "https")
            || !url.username().is_empty()
            || url.password().is_some()
        {
            return Err(ImageGenerationError::local(
                "generated image URL must use HTTP(S) without credentials",
            ));
        }
        // This request intentionally has no API key or custom provider headers, including on the same origin.
        let response = client.get(url).send().await.map_err(transport_error)?;
        if !response.status().is_success() {
            return Err(ImageGenerationError::local(format!(
                "generated image download failed: HTTP {}",
                response.status().as_u16()
            )));
        }
        let bytes = limited_body(response, MAX_IMAGE_BYTES).await?;
        return validate_image(&STANDARD.encode(bytes));
    }
    Err(ImageGenerationError::local(
        "image provider returned no image bytes",
    ))
}

fn aspect_size(
    aspect: Option<&str>,
    prompt: &mut String,
) -> Result<Option<&'static str>, ImageGenerationError> {
    let Some(aspect) = aspect
        .map(str::trim)
        .filter(|s| !s.is_empty() && *s != "auto")
    else {
        return Ok(None);
    };
    let valid = aspect.split_once(':').is_some_and(|(w, h)| {
        w.parse::<u32>().is_ok_and(|v| v > 0 && v <= 100)
            && h.parse::<u32>().is_ok_and(|v| v > 0 && v <= 100)
    });
    if !valid {
        return Err(ImageGenerationError::local("invalid image aspect ratio"));
    }
    prompt.push_str(&format!("\nRequired image aspect ratio: {aspect}."));
    // Only exact standard sizes are mapped; other ratios stay in the prompt for the provider.
    Ok(match aspect {
        "1:1" => Some("1024x1024"),
        "3:2" => Some("1536x1024"),
        "2:3" => Some("1024x1536"),
        _ => None,
    })
}

fn apply_size(
    target: &mut Value,
    aspect: Option<&str>,
    standard_size: Option<&str>,
) -> Result<(), ImageGenerationError> {
    let Some(aspect) = aspect
        .map(str::trim)
        .filter(|s| !s.is_empty() && *s != "auto")
    else {
        return Ok(());
    };
    if let Some(size) = target
        .get("size")
        .and_then(Value::as_str)
        .filter(|s| *s != "auto")
    {
        if let (Some((w, h)), Some((rw, rh))) = (size.split_once('x'), aspect.split_once(':')) {
            if let (Ok(w), Ok(h), Ok(rw), Ok(rh)) = (
                w.parse::<u64>(),
                h.parse::<u64>(),
                rw.parse::<u64>(),
                rh.parse::<u64>(),
            ) {
                if w > 0
                    && h > 0
                    && w.checked_mul(rh)
                        .zip(h.checked_mul(rw))
                        .is_some_and(|(a, b)| a == b)
                {
                    return Ok(());
                }
            }
        }
        if standard_size.is_none() {
            return Err(ImageGenerationError::local("configured image size conflicts with Cursor's aspect ratio; remove size or select a matching size"));
        }
    }
    if let Some(size) = standard_size {
        target["size"] = json!(size);
    }
    Ok(())
}

fn validate_image(data: &str) -> Result<GeneratedImage, ImageGenerationError> {
    if data.len() > MAX_IMAGE_BYTES.div_ceil(3) * 4 {
        return Err(ImageGenerationError::local(
            "generated or reference image exceeds 20 MiB",
        ));
    }
    let bytes = STANDARD
        .decode(data)
        .map_err(|_| ImageGenerationError::local("image provider returned invalid base64"))?;
    let format = image::guess_format(&bytes)
        .map_err(|_| ImageGenerationError::local("image bytes are not a supported image"))?;
    let mime = match format {
        image::ImageFormat::Png => "image/png",
        image::ImageFormat::Jpeg => "image/jpeg",
        image::ImageFormat::WebP => "image/webp",
        _ => {
            return Err(ImageGenerationError::local(
                "only PNG, JPEG and WebP images are supported",
            ))
        }
    };
    let mut reader = image::ImageReader::with_format(Cursor::new(&bytes), format);
    let mut limits = image::Limits::default();
    limits.max_alloc = Some(128 * 1024 * 1024);
    limits.max_image_width = Some(8192);
    limits.max_image_height = Some(8192);
    reader.limits(limits);
    let decoded = reader
        .decode()
        .map_err(|_| ImageGenerationError::local("image is corrupt or exceeds decoding limits"))?;
    Ok(GeneratedImage {
        image_data: STANDARD.encode(&bytes),
        mime_type: mime.into(),
        width: decoded.width(),
        height: decoded.height(),
    })
}

fn transport_error(error: reqwest::Error) -> ImageGenerationError {
    // Request URLs and gateway query credentials never reach UI or logs.
    ImageGenerationError::local(if error.is_timeout() {
        "image provider transport timed out"
    } else if error.is_connect() {
        "cannot connect to image provider"
    } else {
        "image provider transport failed"
    })
}

async fn limited_body(
    response: reqwest::Response,
    limit: usize,
) -> Result<Vec<u8>, ImageGenerationError> {
    if response
        .content_length()
        .is_some_and(|size| size > limit as u64)
    {
        return Err(ImageGenerationError::local(
            "image provider response exceeds size limit",
        ));
    }
    let mut stream = response.bytes_stream();
    let mut bytes = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(transport_error)?;
        if bytes.len() + chunk.len() > limit {
            return Err(ImageGenerationError::local(
                "image provider response exceeds size limit",
            ));
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

fn provider_error(
    model: &ModelConfig,
    request: &ai::RunGenerateImageRequest,
    value: &Value,
    status: u16,
    retry_after: Option<&str>,
) -> ImageGenerationError {
    let code = value
        .pointer("/error/code")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let safety = matches!(
        code,
        "content_policy_violation" | "moderation_blocked" | "content_safety_violation"
    );
    let mut detail = value
        .pointer("/error/message")
        .and_then(Value::as_str)
        .unwrap_or("image generation rejected by provider")
        .to_owned();
    let mut retry = retry_after.unwrap_or_default().to_owned();
    let query_values: Vec<String> = reqwest::Url::parse(&model.base_url)
        .ok()
        .map(|url| {
            url.query_pairs()
                .map(|(_, value)| value.into_owned())
                .collect()
        })
        .unwrap_or_default();
    for secret in std::iter::once(model.api_key.as_str())
        .chain(std::iter::once(request.description.as_str()))
        .chain(
            request
                .reference_images
                .iter()
                .map(|image| image.data.as_str()),
        )
        .chain(
            model
                .custom_headers
                .as_object()
                .into_iter()
                .flat_map(|headers| headers.values().filter_map(Value::as_str)),
        )
        .chain(query_values.iter().map(String::as_str))
    {
        if !secret.is_empty() {
            detail = detail.replace(secret, "[REDACTED]");
            retry = retry.replace(secret, "[REDACTED]");
        }
    }
    detail = detail.chars().take(600).collect();
    let retry = if retry.is_empty() {
        String::new()
    } else {
        format!(
            "; Retry-After: {}",
            retry.chars().take(80).collect::<String>()
        )
    };
    ImageGenerationError {
        message: format!("image provider HTTP {status}: {detail}{retry}"),
        provider_status_code: Some(i32::from(status)),
        content_safety_blocked: safety,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_ratio_preserves_matching_resolution_and_rejects_conflicting_custom_sizes() {
        let mut matching = json!({"size":"2048x3072"});
        apply_size(&mut matching, Some("2:3"), Some("1024x1536")).unwrap();
        assert_eq!(matching["size"], "2048x3072");
        let mut conflicting = json!({"size":"1024x1024"});
        assert!(apply_size(&mut conflicting, Some("16:9"), None).is_err());
        apply_size(&mut conflicting, Some("3:2"), Some("1536x1024")).unwrap();
        assert_eq!(conflicting["size"], "1536x1024");
        let mut unknown = json!({});
        let mut prompt = "draw".into();
        let size = aspect_size(Some("16:9"), &mut prompt).unwrap();
        apply_size(&mut unknown, Some("16:9"), size).unwrap();
        assert!(unknown.get("size").is_none());
        assert!(prompt.contains("16:9"));
        assert!(aspect_size(Some("bogus"), &mut prompt).is_err());
    }
}
