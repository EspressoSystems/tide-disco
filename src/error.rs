use crate::request::RequestError;
use crate::route::RouteError;
use config::ConfigError;
use serde::{de::DeserializeOwned, Serialize};
use std::fmt::Display;
use std::io::Error as IoError;
use tide::StatusCode;

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
        Self::catch_all(StatusCode::InternalServerError, source.to_string())
    }

    fn from_config_error(source: ConfigError) -> Self {
        Self::catch_all(StatusCode::InternalServerError, source.to_string())
    }

    fn from_route_error<E: Display>(source: RouteError<E>) -> Self {
        Self::catch_all(source.status(), source.to_string())
    }

    fn from_request_error(source: RequestError) -> Self {
        Self::catch_all(StatusCode::BadRequest, source.to_string())
    }

    fn into_tide_error(self) -> tide::Error {
        tide::Error::new(self.status(), self)
    }

    fn from_server_error(source: tide::Error) -> Self {
        match source.downcast::<Self>() {
            Ok(err) => err,
            Err(source) => Self::catch_all(source.status(), source.to_string()),
        }
    }
}
