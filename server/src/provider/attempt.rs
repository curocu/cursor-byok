//! Sends one Provider HTTP attempt without applying retry policy.

use tokio_util::sync::CancellationToken;

use crate::{Error, Result};

use super::CallRecorder;

#[derive(Debug)]
pub(crate) enum Attempt {
    Response(reqwest::Response),
    Cancelled,
}

pub(crate) async fn send_once<F>(
    label: &str,
    credential: &str,
    build: F,
    cancellation: &CancellationToken,
    recorder: Option<&CallRecorder>,
) -> Result<Attempt>
where
    F: FnOnce() -> reqwest::RequestBuilder,
{
    let response = tokio::select! {
        _ = cancellation.cancelled() => return Ok(Attempt::Cancelled),
        response = build().send() => response,
    }?;
    if let Some(recorder) = recorder {
        recorder
            .response_headers(response.status().as_u16())
            .await?;
    }
    if response.status().is_success() {
        return Ok(Attempt::Response(response));
    }
    let status = response.status();
    let retry_after = response
        .headers()
        .get(reqwest::header::RETRY_AFTER)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    let bytes = tokio::select! {
        _ = cancellation.cancelled() => return Ok(Attempt::Cancelled),
        bytes = response.bytes() => bytes,
    }?;
    // Error bodies can echo requests. Keep the error message, not the input
    // payload, and redact the credential before it reaches logs or Cursor.
    let detail = match serde_json::from_slice::<serde_json::Value>(&bytes) {
        Ok(value) => value
            .pointer("/error/message")
            .or_else(|| value.get("message"))
            .and_then(serde_json::Value::as_str)
            .or_else(|| value.get("error").and_then(serde_json::Value::as_str))
            .unwrap_or("upstream returned an error without a message")
            .to_owned(),
        Err(_) => String::from_utf8_lossy(&bytes).into_owned(),
    };
    let detail = if credential.is_empty() {
        detail
    } else {
        detail.replace(credential, "[REDACTED]")
    };
    let mut message = format!("{label} {status}: {}", detail.trim());
    if let Some(value) = &retry_after {
        message.push_str(&format!(" (Retry-After: {value})"));
    }
    Err(Error::ProviderHttp {
        status: status.as_u16(),
        message,
        retry_after,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    async fn server(response: &'static [u8]) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = [0_u8; 1024];
            let _ = socket.read(&mut request).await;
            socket.write_all(response).await.unwrap();
        });
        format!("http://{address}")
    }

    #[tokio::test]
    async fn non_success_status_is_one_failed_attempt() {
        let url =
            server(b"HTTP/1.1 503 Service Unavailable\r\nContent-Length: 4\r\n\r\ndown").await;
        let client = reqwest::Client::new();
        let error = send_once(
            "test",
            "",
            || client.get(&url),
            &CancellationToken::new(),
            None,
        )
        .await
        .unwrap_err();
        assert!(
            matches!(error, Error::ProviderHttp { status: 503, message, retry_after: None } if message.contains("503") && message.contains("down"))
        );
    }

    #[tokio::test]
    async fn response_body_transport_failure_is_one_failed_attempt() {
        let url =
            server(b"HTTP/1.1 500 Internal Server Error\r\nContent-Length: 100\r\n\r\nshort").await;
        let client = reqwest::Client::new();
        let error = send_once(
            "test",
            "",
            || client.get(&url),
            &CancellationToken::new(),
            None,
        )
        .await
        .unwrap_err();
        assert!(matches!(error, Error::Http(_)));
    }

    #[tokio::test]
    async fn request_transport_failure_is_one_failed_attempt() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        drop(listener);
        let url = format!("http://{address}");
        let client = reqwest::Client::new();
        let error = send_once(
            "test",
            "",
            || client.get(&url),
            &CancellationToken::new(),
            None,
        )
        .await
        .unwrap_err();
        assert!(matches!(error, Error::Http(_)));
    }

    #[tokio::test]
    async fn http_date_retry_after_and_safe_message_are_preserved() {
        let url = server(b"HTTP/1.1 429 Too Many Requests\r\nRetry-After: Tue, 10 Nov 2026 08:02:00 GMT\r\nConnection: close\r\n\r\n{\"error\":{\"message\":\"quota exceeded test-secret\"},\"input\":\"private prompt\"}").await;
        let client = reqwest::Client::new();
        let error = send_once(
            "test",
            "test-secret",
            || client.get(&url),
            &CancellationToken::new(),
            None,
        )
        .await
        .unwrap_err();
        let Error::ProviderHttp {
            status,
            message,
            retry_after,
        } = error
        else {
            panic!("HTTP details lost")
        };
        assert_eq!(status, 429);
        assert_eq!(
            retry_after.as_deref(),
            Some("Tue, 10 Nov 2026 08:02:00 GMT")
        );
        assert!(message.contains("quota exceeded [REDACTED]"));
        assert!(!message.contains("test-secret"));
        assert!(!message.contains("private prompt"));
    }
}
