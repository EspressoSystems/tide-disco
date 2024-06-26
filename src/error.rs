// Copyright (c) 2022 Espresso Systems (espressosys.com)
// This file is part of the tide-disco library.

// You should have received a copy of the MIT License
// along with the tide-disco library. If not, see <https://mit-license.org/>.

use crate::{request::RequestError, route::RouteError, socket::SocketError, StatusCode};
use config::ConfigError;
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use snafu::Snafu;
use std::fmt::Display;
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

    fn into_tide_error(self) -> tide::Error {
        tide::Error::new(self.status(), self)
    }

    fn from_server_error(source: tide::Error) -> Self {
        match source.downcast::<Self>() {
            Ok(err) => err,
            Err(source) => Self::catch_all(source.status().into(), source.to_string()),
        }
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
