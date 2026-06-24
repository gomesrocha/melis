//! HTTP Client module for LLM providers.
//!
//! Provides a trait-based abstraction over HTTP communication with LLM providers,
//! supporting both non-streaming (complete response) and streaming (SSE) modes.
//!
//! Uses `reqwest` with connection pooling and TLS for efficient, reusable connections.

use std::pin::Pin;
use std::time::Duration;

use async_trait::async_trait;
use bytes::Bytes;
use futures_util::future::Either;
use futures_util::StreamExt;
use reqwest::header::HeaderMap;
use tokio_stream::Stream;

/// Errors that can occur during HTTP communication with LLM providers.
#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    /// The request timed out before receiving a complete response.
    #[error("Request timed out")]
    Timeout,

    /// The provider returned a non-2xx HTTP status code.
    #[error("HTTP error {status}: {body}")]
    HttpError {
        status: u16,
        body: String,
    },

    /// A network-level error occurred (DNS resolution, connection refused, etc.).
    #[error("Network error: {0}")]
    NetworkError(String),

    /// An error occurred while reading the streaming response.
    #[error("Stream error: {0}")]
    StreamError(String),
}

/// Trait defining the HTTP client interface for LLM providers.
///
/// Implementors handle the low-level HTTP communication, including
/// connection pooling, TLS, timeouts, and streaming support.
#[async_trait]
pub trait LlmHttpClient: Send + Sync {
    /// Sends a non-streaming POST request to the provider.
    ///
    /// Waits for the complete response body before returning.
    ///
    /// # Arguments
    /// * `url` - The full URL to send the request to
    /// * `headers` - HTTP headers to include (e.g., Authorization, Content-Type)
    /// * `body` - The request body bytes
    /// * `timeout` - Maximum duration to wait for the response
    ///
    /// # Returns
    /// The complete response body as `Bytes`, or a `ClientError` on failure.
    async fn send(
        &self,
        url: &str,
        headers: HeaderMap,
        body: Bytes,
        timeout: Duration,
    ) -> Result<Bytes, ClientError>;

    /// Sends a streaming POST request to the provider, returning an SSE stream.
    ///
    /// The returned stream yields raw SSE data chunks split by double newlines (`\n\n`).
    ///
    /// # Arguments
    /// * `url` - The full URL to send the request to
    /// * `headers` - HTTP headers to include (e.g., Authorization, Content-Type)
    /// * `body` - The request body bytes
    /// * `timeout` - Maximum duration to wait for the initial connection
    ///
    /// # Returns
    /// A pinned stream of `Result<Bytes, ClientError>` items.
    fn send_stream(
        &self,
        url: &str,
        headers: HeaderMap,
        body: Bytes,
        timeout: Duration,
    ) -> Pin<Box<dyn Stream<Item = Result<Bytes, ClientError>> + Send>>;
}

/// Production HTTP client implementation using `reqwest`.
///
/// Maintains a shared `reqwest::Client` internally for connection pooling
/// and TLS session reuse across requests.
#[derive(Debug, Clone)]
pub struct ReqwestLlmClient {
    client: reqwest::Client,
}

impl ReqwestLlmClient {
    /// Creates a new `ReqwestLlmClient` with default configuration.
    ///
    /// The underlying `reqwest::Client` is configured with connection pooling
    /// and TLS enabled by default.
    pub fn new() -> Self {
        let client = reqwest::Client::new();
        Self { client }
    }
}

impl Default for ReqwestLlmClient {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl LlmHttpClient for ReqwestLlmClient {
    async fn send(
        &self,
        url: &str,
        headers: HeaderMap,
        body: Bytes,
        timeout: Duration,
    ) -> Result<Bytes, ClientError> {
        let response = self
            .client
            .post(url)
            .headers(headers)
            .body(body)
            .timeout(timeout)
            .send()
            .await
            .map_err(|e| {
                if e.is_timeout() {
                    ClientError::Timeout
                } else {
                    ClientError::NetworkError(e.to_string())
                }
            })?;

        let status = response.status();
        if !status.is_success() {
            let body_text = response
                .text()
                .await
                .unwrap_or_else(|_| "Failed to read error body".to_string());
            return Err(ClientError::HttpError {
                status: status.as_u16(),
                body: body_text,
            });
        }

        response
            .bytes()
            .await
            .map_err(|e| ClientError::NetworkError(e.to_string()))
    }

    fn send_stream(
        &self,
        url: &str,
        headers: HeaderMap,
        body: Bytes,
        timeout: Duration,
    ) -> Pin<Box<dyn Stream<Item = Result<Bytes, ClientError>> + Send>> {
        let client = self.client.clone();
        let url = url.to_string();

        let stream = async_stream(client, url, headers, body, timeout);
        Box::pin(stream)
    }
}

/// Creates an async stream that connects to the provider and yields SSE data chunks.
///
/// Each item in the stream is a raw SSE event (split by `\n\n` boundaries).
fn async_stream(
    client: reqwest::Client,
    url: String,
    headers: HeaderMap,
    body: Bytes,
    timeout: Duration,
) -> impl Stream<Item = Result<Bytes, ClientError>> + Send {
    futures_util::stream::once(async move {
        let response = client
            .post(&url)
            .headers(headers)
            .body(body)
            .timeout(timeout)
            .send()
            .await
            .map_err(|e| {
                if e.is_timeout() {
                    ClientError::Timeout
                } else {
                    ClientError::NetworkError(e.to_string())
                }
            })?;

        let status = response.status();
        if !status.is_success() {
            let body_text = response
                .text()
                .await
                .unwrap_or_else(|_| "Failed to read error body".to_string());
            return Err(ClientError::HttpError {
                status: status.as_u16(),
                body: body_text,
            });
        }

        Ok(response)
    })
    .filter_map(|result| async move {
        match result {
            Ok(response) => Some(Ok(response)),
            Err(e) => Some(Err(e)),
        }
    })
    .flat_map(|result| {
        match result {
            Ok(response) => {
                // Convert the response byte stream into SSE event chunks
                let byte_stream = response.bytes_stream();
                let sse_stream = SseChunkSplitter::new(byte_stream);
                Either::Left(sse_stream)
            }
            Err(e) => {
                Either::Right(futures_util::stream::once(async move {
                    Err(e)
                }))
            }
        }
    })
}

/// Splits a raw byte stream into SSE event chunks delimited by `\n\n`.
///
/// Buffers incoming bytes and yields complete SSE events when a double-newline
/// boundary is detected.
struct SseChunkSplitter<S> {
    inner: S,
    buffer: Vec<u8>,
}

impl<S> SseChunkSplitter<S> {
    fn new(inner: S) -> Self {
        Self {
            inner,
            buffer: Vec::new(),
        }
    }
}

impl<S> Stream for SseChunkSplitter<S>
where
    S: Stream<Item = Result<Bytes, reqwest::Error>> + Unpin,
{
    type Item = Result<Bytes, ClientError>;

    fn poll_next(
        self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        let this = self.get_mut();

        loop {
            // Check if we have a complete SSE event in the buffer
            if let Some(pos) = find_double_newline(&this.buffer) {
                let event_bytes = this.buffer.drain(..pos + 2).collect::<Vec<u8>>();
                return std::task::Poll::Ready(Some(Ok(Bytes::from(event_bytes))));
            }

            // Poll the inner stream for more data
            let inner = Pin::new(&mut this.inner);
            match inner.poll_next(cx) {
                std::task::Poll::Ready(Some(Ok(chunk))) => {
                    this.buffer.extend_from_slice(&chunk);
                    // Loop back to check for complete events
                }
                std::task::Poll::Ready(Some(Err(e))) => {
                    return std::task::Poll::Ready(Some(Err(ClientError::StreamError(
                        e.to_string(),
                    ))));
                }
                std::task::Poll::Ready(None) => {
                    // Stream ended - flush any remaining buffered data
                    if !this.buffer.is_empty() {
                        let remaining = this.buffer.drain(..).collect::<Vec<u8>>();
                        return std::task::Poll::Ready(Some(Ok(Bytes::from(remaining))));
                    }
                    return std::task::Poll::Ready(None);
                }
                std::task::Poll::Pending => {
                    return std::task::Poll::Pending;
                }
            }
        }
    }
}

/// Finds the position of the first `\n\n` double-newline in a byte slice.
///
/// Returns the index of the first `\n` in the `\n\n` pair if found.
fn find_double_newline(buf: &[u8]) -> Option<usize> {
    buf.windows(2).position(|w| w == b"\n\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_error_timeout_display() {
        let err = ClientError::Timeout;
        assert_eq!(format!("{}", err), "Request timed out");
    }

    #[test]
    fn client_error_http_error_display() {
        let err = ClientError::HttpError {
            status: 429,
            body: "Rate limited".to_string(),
        };
        assert_eq!(format!("{}", err), "HTTP error 429: Rate limited");
    }

    #[test]
    fn client_error_network_error_display() {
        let err = ClientError::NetworkError("connection refused".to_string());
        assert_eq!(format!("{}", err), "Network error: connection refused");
    }

    #[test]
    fn client_error_stream_error_display() {
        let err = ClientError::StreamError("broken pipe".to_string());
        assert_eq!(format!("{}", err), "Stream error: broken pipe");
    }

    #[test]
    fn client_error_debug() {
        let err = ClientError::Timeout;
        let debug = format!("{:?}", err);
        assert!(debug.contains("Timeout"));

        let err = ClientError::HttpError {
            status: 500,
            body: "Internal Server Error".to_string(),
        };
        let debug = format!("{:?}", err);
        assert!(debug.contains("HttpError"));
        assert!(debug.contains("500"));
    }

    #[test]
    fn reqwest_client_creation() {
        let client = ReqwestLlmClient::new();
        // Verify the client was created (no panic)
        let _ = format!("{:?}", client);
    }

    #[test]
    fn reqwest_client_default() {
        let client = ReqwestLlmClient::default();
        let _ = format!("{:?}", client);
    }

    #[test]
    fn reqwest_client_clone() {
        let client = ReqwestLlmClient::new();
        let cloned = client.clone();
        let _ = format!("{:?}", cloned);
    }

    #[test]
    fn find_double_newline_found() {
        let data = b"data: hello\n\ndata: world\n\n";
        assert_eq!(find_double_newline(data), Some(11));
    }

    #[test]
    fn find_double_newline_not_found() {
        let data = b"data: hello\ndata: world\n";
        assert_eq!(find_double_newline(data), None);
    }

    #[test]
    fn find_double_newline_at_start() {
        let data = b"\n\ndata: hello";
        assert_eq!(find_double_newline(data), Some(0));
    }

    #[test]
    fn find_double_newline_empty() {
        let data = b"";
        assert_eq!(find_double_newline(data), None);
    }
}
