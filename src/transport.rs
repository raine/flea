use std::{
    collections::VecDeque,
    fmt,
    future::Future,
    pin::Pin,
    sync::{Arc, Mutex},
    time::Duration,
};

use reqwest::{Method, StatusCode, header::HeaderMap};

#[derive(Clone, Eq, PartialEq)]
pub struct MultipartPart {
    pub name: String,
    pub file_name: Option<String>,
    pub mime_type: Option<String>,
    bytes: Vec<u8>,
}

impl MultipartPart {
    pub fn bytes(name: impl Into<String>, bytes: impl Into<Vec<u8>>) -> Self {
        Self {
            name: name.into(),
            file_name: None,
            mime_type: None,
            bytes: bytes.into(),
        }
    }

    pub fn file_name(mut self, file_name: impl Into<String>) -> Self {
        self.file_name = Some(file_name.into());
        self
    }

    pub fn mime_type(mut self, mime_type: impl Into<String>) -> Self {
        self.mime_type = Some(mime_type.into());
        self
    }

    pub fn len(&self) -> usize {
        self.bytes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.bytes.is_empty()
    }
}

impl fmt::Debug for MultipartPart {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("MultipartPart")
            .field("name", &self.name)
            .field("file_name", &self.file_name)
            .field("mime_type", &self.mime_type)
            .field("byte_len", &self.bytes.len())
            .finish()
    }
}

#[derive(Clone, Default, Eq, PartialEq)]
pub enum RequestBody {
    #[default]
    Empty,
    Bytes(Vec<u8>),
    Multipart(Vec<MultipartPart>),
}

impl fmt::Debug for RequestBody {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => formatter.write_str("Empty"),
            Self::Bytes(bytes) => formatter
                .debug_struct("Bytes")
                .field("byte_len", &bytes.len())
                .finish(),
            Self::Multipart(parts) => formatter.debug_tuple("Multipart").field(parts).finish(),
        }
    }
}

#[derive(Clone)]
pub struct TransportRequest {
    pub method: Method,
    pub url: String,
    pub headers: HeaderMap,
    pub body: RequestBody,
    pub deadline: Duration,
    pub max_response_bytes: usize,
}

impl fmt::Debug for TransportRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TransportRequest")
            .field("method", &self.method)
            .field("request_target", &"[REDACTED]")
            .field("header_names", &header_names(&self.headers))
            .field("body", &self.body)
            .field("deadline", &self.deadline)
            .field("max_response_bytes", &self.max_response_bytes)
            .finish()
    }
}

#[derive(Clone)]
pub struct TransportResponse {
    pub status: StatusCode,
    pub headers: HeaderMap,
    pub body: Vec<u8>,
}

impl fmt::Debug for TransportResponse {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TransportResponse")
            .field("status", &self.status)
            .field("header_names", &header_names(&self.headers))
            .field("body_len", &self.body.len())
            .finish()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TransportErrorKind {
    Timeout,
    Connection,
    ResponseTooLarge,
    Other,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TransportErrorPhase {
    Request,
    Response,
}

#[derive(Clone, thiserror::Error, Eq, PartialEq)]
#[error("HTTP transport {kind}")]
pub struct TransportError {
    pub kind: TransportErrorKind,
    pub phase: TransportErrorPhase,
    pub status: Option<StatusCode>,
}

impl TransportError {
    pub const fn request(kind: TransportErrorKind) -> Self {
        Self {
            kind,
            phase: TransportErrorPhase::Request,
            status: None,
        }
    }

    pub const fn response(kind: TransportErrorKind, status: StatusCode) -> Self {
        Self {
            kind,
            phase: TransportErrorPhase::Response,
            status: Some(status),
        }
    }
}

impl fmt::Debug for TransportError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TransportError")
            .field("kind", &self.kind)
            .field("phase", &self.phase)
            .field("status", &self.status)
            .finish()
    }
}

impl fmt::Display for TransportErrorKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Timeout => "timed out",
            Self::Connection => "connection failed",
            Self::ResponseTooLarge | Self::Other => "failed",
        })
    }
}

pub type TransportFuture<'a> =
    Pin<Box<dyn Future<Output = Result<TransportResponse, TransportError>> + Send + 'a>>;

pub trait Transport: Send + Sync {
    fn execute(&self, request: TransportRequest) -> TransportFuture<'_>;
}

#[derive(Clone)]
pub struct ReqwestTransport {
    client: reqwest::Client,
}

impl Default for ReqwestTransport {
    fn default() -> Self {
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never())
            .build()
            .expect("static HTTP client configuration is valid");
        Self { client }
    }
}

impl Transport for ReqwestTransport {
    fn execute(&self, request: TransportRequest) -> TransportFuture<'_> {
        Box::pin(async move {
            let TransportRequest {
                method,
                url,
                headers,
                body,
                deadline,
                max_response_bytes,
            } = request;
            let mut builder = self
                .client
                .request(method, url)
                .headers(headers)
                .timeout(deadline);
            builder = match body {
                RequestBody::Empty => builder,
                RequestBody::Bytes(bytes) => builder.body(bytes),
                RequestBody::Multipart(parts) => {
                    let mut form = reqwest::multipart::Form::new();
                    for part in parts {
                        let mut reqwest_part = reqwest::multipart::Part::bytes(part.bytes);
                        if let Some(file_name) = part.file_name {
                            reqwest_part = reqwest_part.file_name(file_name);
                        }
                        if let Some(mime_type) = part.mime_type {
                            reqwest_part = reqwest_part
                                .mime_str(&mime_type)
                                .map_err(|_| TransportError::request(TransportErrorKind::Other))?;
                        }
                        form = form.part(part.name, reqwest_part);
                    }
                    builder.multipart(form)
                }
            };

            let mut response = builder.send().await.map_err(classify_reqwest_error)?;
            if response
                .content_length()
                .is_some_and(|len| len > max_response_bytes as u64)
            {
                return Err(TransportError::response(
                    TransportErrorKind::ResponseTooLarge,
                    response.status(),
                ));
            }

            let status = response.status();
            let headers = response.headers().clone();
            let mut bytes = Vec::new();
            while let Some(chunk) = response
                .chunk()
                .await
                .map_err(|error| classify_response_error(error, status))?
            {
                if bytes.len().saturating_add(chunk.len()) > max_response_bytes {
                    return Err(TransportError::response(
                        TransportErrorKind::ResponseTooLarge,
                        status,
                    ));
                }
                bytes.extend_from_slice(&chunk);
            }

            Ok(TransportResponse {
                status,
                headers,
                body: bytes,
            })
        })
    }
}

#[derive(Clone)]
pub struct RecordingTransport {
    requests: Arc<Mutex<Vec<TransportRequest>>>,
    responses: Arc<Mutex<VecDeque<Result<TransportResponse, TransportError>>>>,
}

impl RecordingTransport {
    pub fn queued(
        responses: impl IntoIterator<Item = Result<TransportResponse, TransportError>>,
    ) -> Self {
        Self {
            requests: Arc::new(Mutex::new(Vec::new())),
            responses: Arc::new(Mutex::new(responses.into_iter().collect())),
        }
    }

    pub fn requests(&self) -> Vec<TransportRequest> {
        self.requests
            .lock()
            .expect("request recording lock")
            .clone()
    }
}

impl Transport for RecordingTransport {
    fn execute(&self, request: TransportRequest) -> TransportFuture<'_> {
        self.requests
            .lock()
            .expect("request recording lock")
            .push(request);
        let response = self
            .responses
            .lock()
            .expect("response queue lock")
            .pop_front()
            .unwrap_or(Err(TransportError::request(TransportErrorKind::Other)));
        Box::pin(async move { response })
    }
}

fn classify_reqwest_error(error: reqwest::Error) -> TransportError {
    TransportError::request(reqwest_error_kind(&error))
}

fn classify_response_error(error: reqwest::Error, status: StatusCode) -> TransportError {
    TransportError::response(reqwest_error_kind(&error), status)
}

fn reqwest_error_kind(error: &reqwest::Error) -> TransportErrorKind {
    if error.is_timeout() {
        TransportErrorKind::Timeout
    } else if error.is_connect() {
        TransportErrorKind::Connection
    } else {
        TransportErrorKind::Other
    }
}

fn header_names(headers: &HeaderMap) -> Vec<&str> {
    headers
        .keys()
        .map(reqwest::header::HeaderName::as_str)
        .collect()
}
