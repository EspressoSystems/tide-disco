use crate::{request::RequestError, status::StatusCode};
use config::ConfigError;
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use snafu::Snafu;
use std::fmt::{self, Display, Formatter};
use std::io::Error as IoError;

/// Errors which can be serialized in a response body.
///
/// This trait can be used to define a standard error type returned by all API endpoints. When a
/// request fails for any reason, the body of the response will contain a serialization of
/// the error that caused the failure, upcasted into an anyhow::Error. If the error is an instance
/// of the standard error type for that particular API, it can be deserialized and downcasted to
/// this type on the client.
///
/// Other errors (those which don't downcast to the API's error type, such as errors generated from
/// the [tide] framework) will be serialized as strings using their [Display] instance and encoded
/// as an API error using the [catch_all](Error::catch_all) function.
pub trait Error: std::error::Error + Serialize + DeserializeOwned + Send + Sync + 'static {
    fn catch_all(status: StatusCode, msg: String) -> Self;
    fn status(&self) -> StatusCode;

    fn message(&self) -> String {
        self.to_string()
    }

    fn from_io_error(source: IoError) -> Self {
        Self::catch_all(StatusCode::INTERNAL_SERVER_ERROR, source.to_string())
    }

    fn from_config_error(source: ConfigError) -> Self {
        Self::catch_all(StatusCode::INTERNAL_SERVER_ERROR, source.to_string())
    }

    fn from_route_error<E: Display>(source: RouteError<E>) -> Self {
        Self::catch_all(source.status(), source.to_string())
    }

    fn from_request_error(source: RequestError) -> Self {
        Self::catch_all(StatusCode::BAD_REQUEST, source.to_string())
    }

    fn from_socket_error<E: Display>(source: SocketError<E>) -> Self {
        Self::catch_all(source.status(), source.to_string())
    }
}

/// The simplest possible implementation of [Error].
///
/// You can use this to get up and running quickly if you don't want to create your own error type.
/// However, we strongly reccommend creating a custom error type and implementing [Error] for it, so
/// that you can provide more informative and structured error responses specific to your API.
#[derive(Clone, Debug, Snafu, Serialize, Deserialize, PartialEq, Eq)]
#[snafu(display("Error {}: {}", status, message))]
pub struct ServerError {
    pub status: StatusCode,
    pub message: String,
}

impl Error for ServerError {
    fn catch_all(status: StatusCode, message: String) -> Self {
        Self { status, message }
    }

    fn status(&self) -> StatusCode {
        self.status
    }

    fn message(&self) -> String {
        self.message.clone()
    }
}

impl From<IoError> for ServerError {
    fn from(source: IoError) -> Self {
        Self::from_io_error(source)
    }
}

impl From<ConfigError> for ServerError {
    fn from(source: ConfigError) -> Self {
        Self::from_config_error(source)
    }
}

impl<E: Display> From<RouteError<E>> for ServerError {
    fn from(source: RouteError<E>) -> Self {
        Self::from_route_error(source)
    }
}

impl From<RequestError> for ServerError {
    fn from(source: RequestError) -> Self {
        Self::from_request_error(source)
    }
}

impl<E: Display> From<SocketError<E>> for ServerError {
    fn from(source: SocketError<E>) -> Self {
        Self::from_socket_error(source)
    }
}

/// An error returned by a route handler.
///
/// [RouteError] encapsulates application specific errors `E` returned by the user-installed handler
/// itself. It also includes errors in the route dispatching logic, such as failures to turn the
/// result of the user-installed handler into an HTTP response.
#[derive(Debug)]
pub enum RouteError<E> {
    AppSpecific(E),
    Request(RequestError),
    UnsupportedContentType,
    Binary(anyhow::Error),
    Json(serde_json::Error),
    Tide { status: StatusCode, message: String },
    ExportMetrics(String),
    IncorrectMethod { expected: String },
}

impl<E: Display> Display for RouteError<E> {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        match self {
            Self::AppSpecific(err) => write!(f, "{}", err),
            Self::Request(err) => write!(f, "{}", err),
            Self::UnsupportedContentType => write!(f, "requested content type is not supported"),
            Self::Binary(err) => write!(f, "error creating byte stream: {}", err),
            Self::Json(err) => write!(f, "error creating JSON response: {}", err),
            Self::Tide { status, message } => write!(f, "{status}: {message}"),
            Self::ExportMetrics(msg) => write!(f, "error exporting metrics: {msg}"),
            Self::IncorrectMethod { expected } => {
                write!(f, "route may only be called as {}", expected)
            }
        }
    }
}

impl<E> RouteError<E> {
    pub fn status(&self) -> StatusCode {
        match self {
            Self::Request(_) | Self::UnsupportedContentType | Self::IncorrectMethod { .. } => {
                StatusCode::BAD_REQUEST
            }
            _ => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }

    pub fn map_app_specific<E2>(self, f: impl Fn(E) -> E2) -> RouteError<E2> {
        match self {
            RouteError::AppSpecific(e) => RouteError::AppSpecific(f(e)),
            RouteError::Request(e) => RouteError::Request(e),
            RouteError::UnsupportedContentType => RouteError::UnsupportedContentType,
            RouteError::Binary(err) => RouteError::Binary(err),
            RouteError::Json(err) => RouteError::Json(err),
            RouteError::Tide { status, message } => RouteError::Tide { status, message },
            RouteError::ExportMetrics(msg) => RouteError::ExportMetrics(msg),
            Self::IncorrectMethod { expected } => RouteError::IncorrectMethod { expected },
        }
    }
}

impl<E> From<RequestError> for RouteError<E> {
    fn from(err: RequestError) -> Self {
        Self::Request(err)
    }
}

/// An error returned by a socket handler.
///
/// [SocketError] encapsulates application specific errors `E` returned by the user-installed
/// handler itself. It also includes errors in the socket protocol, such as failures to turn
/// messages sent by the user-installed handler into WebSockets messages.
#[derive(Debug)]
pub enum SocketError<E> {
    AppSpecific(E),
    Request(RequestError),
    Binary(anyhow::Error),
    Json(serde_json::Error),
    WebSockets(String),
    UnsupportedMessageType,
    Closed,
    IncorrectMethod { expected: String, actual: String },
}

impl<E> SocketError<E> {
    pub fn status(&self) -> StatusCode {
        match self {
            Self::Request(_) | Self::UnsupportedMessageType | Self::IncorrectMethod { .. } => {
                StatusCode::BAD_REQUEST
            }
            _ => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }

    pub fn map_app_specific<E2>(self, f: &impl Fn(E) -> E2) -> SocketError<E2> {
        match self {
            Self::AppSpecific(e) => SocketError::AppSpecific(f(e)),
            Self::Request(e) => SocketError::Request(e),
            Self::Binary(e) => SocketError::Binary(e),
            Self::Json(e) => SocketError::Json(e),
            Self::WebSockets(e) => SocketError::WebSockets(e),
            Self::UnsupportedMessageType => SocketError::UnsupportedMessageType,
            Self::Closed => SocketError::Closed,
            Self::IncorrectMethod { expected, actual } => {
                SocketError::IncorrectMethod { expected, actual }
            }
        }
    }
}

impl<E: Display> Display for SocketError<E> {
    fn fmt(&self, f: &mut Formatter) -> fmt::Result {
        match self {
            Self::AppSpecific(e) => write!(f, "{}", e),
            Self::Request(e) => write!(f, "{}", e),
            Self::Binary(e) => write!(f, "error creating byte stream: {}", e),
            Self::Json(e) => write!(f, "error creating JSON message: {}", e),
            Self::WebSockets(e) => write!(f, "WebSockets protocol error: {}", e),
            Self::UnsupportedMessageType => {
                write!(f, "unsupported content type for WebSockets message")
            }
            Self::Closed => write!(f, "connection closed"),
            Self::IncorrectMethod { expected, actual } => write!(
                f,
                "endpoint must be called as {}, but was called as {}",
                expected, actual
            ),
        }
    }
}

impl<E> From<RequestError> for SocketError<E> {
    fn from(err: RequestError) -> Self {
        Self::Request(err)
    }
}

impl<E> From<anyhow::Error> for SocketError<E> {
    fn from(err: anyhow::Error) -> Self {
        Self::Binary(err)
    }
}

impl<E> From<serde_json::Error> for SocketError<E> {
    fn from(err: serde_json::Error) -> Self {
        Self::Json(err)
    }
}
