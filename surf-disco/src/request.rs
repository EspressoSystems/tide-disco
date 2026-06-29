// Copyright (c) 2022 Espresso Systems (espressosys.com)
// This file is part of the surf-disco library.

// You should have received a copy of the MIT License
// along with the surf-disco library. If not, see <https://mit-license.org/>.

use crate::{
    http::headers::{HeaderName, ToHeaderValues},
    Error, StatusCode,
};
use serde::{de::DeserializeOwned, Serialize};
use std::{error::Error as _, fmt::Display};
use vbs::{version::StaticVersionType, BinarySerializer, Serializer};

#[must_use]
#[derive(Debug)]
pub struct Request<T, E, VER: StaticVersionType> {
    inner: reqwest::RequestBuilder,
    marker: std::marker::PhantomData<fn(T, E, VER) -> ()>,
}

impl<T, E, VER: StaticVersionType> From<reqwest::RequestBuilder> for Request<T, E, VER> {
    fn from(inner: reqwest::RequestBuilder) -> Self {
        Self {
            inner,
            marker: Default::default(),
        }
    }
}

impl<T: DeserializeOwned, E: Error, VER: StaticVersionType> Request<T, E, VER> {
    /// Set a header on the request.
    pub fn header(mut self, key: impl Into<HeaderName>, values: impl ToHeaderValues) -> Self {
        let key = reqwest::header::HeaderName::from_bytes(key.into().as_str().as_bytes()).unwrap();
        for value in values.to_header_values().unwrap() {
            self = self.inner.header(key.clone(), value.as_str()).into()
        }
        self
    }

    /// Set the request body using JSON.
    ///
    /// Body is serialized using [serde_json] and the `Content-Type` header is set to
    /// `application/json`.
    pub fn body_json<B: Serialize>(self, body: &B) -> Result<Self, E> {
        Ok(self
            .header("Content-Type", "application/json")
            .inner
            .body(serde_json::to_string(body).map_err(request_error)?)
            .into())
    }

    /// Set the request body using [bincode].
    ///
    /// Body is serialized using [bincode] and the `Content-Type` header is set to
    /// `application/octet-stream`.
    ///
    /// # Errors
    ///
    /// Fails if `body` does not serialize successfully.
    pub fn body_binary<B: Serialize>(self, body: &B) -> Result<Self, E> {
        Ok(self
            .header("Content-Type", "application/octet-stream")
            .inner
            .body(Serializer::<VER>::serialize(body).map_err(request_error)?)
            .into())
    }

    /// Send the request and await a response from the server.
    ///
    /// If the request succeeds (receives a response with [StatusCode::OK]) the response body is
    /// converted to a `T`, using a format determined by the `Content-Type` header of the request.
    ///
    /// # Errors
    ///
    /// If the client is unable to reach the server, or if the response body cannot be interpreted
    /// as a `T`, an error message is created using [catch_all](Error::catch_all) and returned.
    ///
    /// If the request completes but the response status code is not [StatusCode::OK], an error
    /// message is constructed using the body of the response. If there is a body and it can be
    /// converted to an `E` using the content type specified in the response's `Content-Type`
    /// header, that `E` will be returned directly. Otherwise, an error message is synthesized using
    /// [catch_all](Error::catch_all) that includes human-readable information about the response.
    pub async fn send(self) -> Result<T, E> {
        let res = self.inner.send().await.map_err(reqwest_error)?;
        let status = res.status();
        let content_type = res.headers().get("Content-Type").cloned();
        if res.status() == StatusCode::OK {
            // If the response indicates success, deserialize the body using a format determined by
            // the Content-Type header.
            if let Some(content_type) = content_type {
                match content_type.to_str() {
                    Ok("application/json") => res.json().await.map_err(reqwest_error),
                    Ok("application/octet-stream") => {
                        Serializer::<VER>::deserialize(&res.bytes().await.map_err(reqwest_error)?)
                            .map_err(request_error)
                    }
                    content_type => {
                        // For help in debugging, include the body with the unexpected content type
                        // in the error message.
                        let msg = match res.bytes().await {
                            Ok(bytes) => match std::str::from_utf8(&bytes) {
                                Ok(s) => format!("body: {}", s),
                                Err(_) => format!("body: {}", hex::encode(&bytes)),
                            },
                            Err(_) => String::default(),
                        };
                        Err(E::catch_all(
                            StatusCode::UNSUPPORTED_MEDIA_TYPE,
                            format!("unsupported content type {content_type:?} {msg}"),
                        ))
                    }
                }
            } else {
                Err(E::catch_all(
                    StatusCode::UNSUPPORTED_MEDIA_TYPE,
                    "unspecified content type in response".into(),
                ))
            }
        } else {
            // To add context to the error, try to interpret the response body as a serialized
            // error. Since `body_json`, `body_string`, etc. consume the response body, we will
            // extract the body as raw bytes and then try various potential decodings based on the
            // response headers and the contents of the body.
            let bytes = match res.bytes().await {
                Ok(bytes) => bytes,
                Err(err) => {
                    // If we are unable to even read the body, just return a generic error message
                    // based on the status code.
                    return Err(E::catch_all(
                        status.into(),
                        format!(
                            "Request terminated with error {status}. Failed to read request body due to {err}",
                        ),
                    ));
                }
            };
            if let Some(content_type) = &content_type {
                // If the response specifies a content type, check if it is one of the types we know
                // how to deserialize, and if it is, we can then see if it deserializes to an `E`.
                match content_type.to_str() {
                    Ok("application/json") => {
                        if let Ok(err) = serde_json::from_slice(&bytes) {
                            return Err(err);
                        }
                    }
                    Ok("application/octet-stream") => {
                        if let Ok(err) = Serializer::<VER>::deserialize(&bytes) {
                            return Err(err);
                        }
                    }
                    _ => {}
                }
            }
            // If we get here, then we were not able to interpret the response body as an `E`
            // directly. This can be because:
            //  * the content type is not supported for deserialization
            //  * the content type was unspecified
            //  * the body did not deserialize to an `E` We have one thing left we can try: if the
            //    body is a string, we can use the `catch_all` variant of `E` to include the
            //    contents of the string in the error message.
            if let Ok(msg) = std::str::from_utf8(&bytes) {
                return Err(E::catch_all(status.into(), msg.to_string()));
            }

            // The response body was not an `E` or a string. Return the most helpful error message
            // we can, including the status code, content type, and raw body.
            Err(E::catch_all(
                status.into(),
                format!(
                    "Request terminated with error {status}. Content-Type: {}. Body: 0x{}",
                    match content_type {
                        Some(content_type) =>
                            content_type.to_str().unwrap_or("unspecified").to_owned(),
                        None => "unspecified".to_owned(),
                    },
                    hex::encode(&bytes)
                ),
            ))
        }
    }

    /// Sends the request and returns the full response body as raw bytes,
    pub async fn bytes(self) -> Result<Vec<u8>, E> {
        let response = self.inner.send().await.map_err(reqwest_error)?;
        let status = response.status();
        let content_type = response.headers().get("Content-Type").cloned();

        let bytes_result = response.bytes().await.map(|b| b.to_vec()).map_err(|err| E::catch_all(
                status.into(),
                format!(
                    "Request terminated with error {status}. Failed to read request body due to {err}",
                ),
            )
        );

        if status.is_success() {
            return bytes_result;
        }

        let bytes = bytes_result?;

        if let Ok(msg) = std::str::from_utf8(&bytes) {
            return Err(E::catch_all(status.into(), msg.to_string()));
        }

        Err(E::catch_all(
            status.into(),
            format!(
                "Request failed with status {status}. Content-Type: {}. Body: 0x{}",
                content_type
                    .as_ref()
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or("unspecified"),
                hex::encode(&bytes),
            ),
        ))
    }
}

fn request_error<E: Error>(source: impl Display) -> E {
    E::catch_all(StatusCode::BAD_REQUEST, source.to_string())
}

fn reqwest_error<E: Error>(source: reqwest::Error) -> E {
    E::catch_all(
        source
            .status()
            .unwrap_or(reqwest::StatusCode::INTERNAL_SERVER_ERROR)
            .into(),
        reqwest_error_msg(source),
    )
}

pub(crate) fn reqwest_error_msg(err: reqwest::Error) -> String {
    match err.source() {
        Some(inner) => format!("{err}: {inner}"),
        None => err.to_string(),
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::{Client, ContentType};
    use async_compatibility_layer::logging::{setup_backtrace, setup_logging};
    use async_std::task::spawn;
    use futures::FutureExt;
    use portpicker::pick_unused_port;
    use tide_disco::{error::ServerError, App};
    use toml::toml;
    use vbs::version::StaticVersion;

    type Ver01 = StaticVersion<0, 1>;
    const VER_0_1: Ver01 = StaticVersion {};

    #[async_std::test]
    async fn test_request_accept() {
        setup_logging();
        setup_backtrace();

        // Set up a simple Tide Disco app.
        let mut app: App<(), ServerError> = App::with_state(());
        let api = toml! {
            [route.get]
            PATH = ["/get"]
        };
        app.module::<ServerError, Ver01>("mod", api)
            .unwrap()
            .get("get", |_req, _state| async move { Ok("response") }.boxed())
            .unwrap();
        let port = pick_unused_port().unwrap();
        spawn(app.serve(format!("0.0.0.0:{port}"), VER_0_1));

        // Connect one client with each supported content type.
        let json_client = Client::<ServerError, Ver01>::builder(
            format!("http://localhost:{port}").parse().unwrap(),
        )
        .content_type(ContentType::Json)
        .build();
        assert!(json_client.connect(None).await);

        let bin_client = Client::<ServerError, Ver01>::builder(
            format!("http://localhost:{port}").parse().unwrap(),
        )
        .content_type(ContentType::Binary)
        .build();
        assert!(bin_client.connect(None).await);

        // Check that requests built with each client get a response in the desired content type.
        let res = json_client
            .get::<String>("mod/get")
            .inner
            .send()
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(res.headers()["Content-Type"], "application/json");
        assert_eq!(res.json::<String>().await.unwrap(), "response");

        let res = bin_client
            .get::<String>("mod/get")
            .inner
            .send()
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(res.headers()["Content-Type"], "application/octet-stream");
        assert_eq!(
            Serializer::<Ver01>::deserialize::<String>(&res.bytes().await.unwrap()).unwrap(),
            "response"
        );
    }

    #[async_std::test]
    async fn test_bad_response_bytes() {
        setup_logging();
        setup_backtrace();

        // Set up a simple Tide Disco app.
        let mut app: App<(), ServerError> = App::with_state(());
        let api = toml! {
            [route.integer]
            PATH = ["/integer"]
        };

        app.module::<ServerError, Ver01>("app", api)
            .unwrap()
            .get::<_, u64>("integer", |_req, _state| {
                async move {
                    Err(ServerError::catch_all(
                        StatusCode::NOT_FOUND,
                        "not found".to_string(),
                    ))
                }
                .boxed()
            })
            .unwrap();

        let port = pick_unused_port().unwrap();
        spawn(app.serve(format!("0.0.0.0:{port}"), VER_0_1));

        // Connect client
        let client = Client::<ServerError, Ver01>::builder(
            format!("http://localhost:{port}").parse().unwrap(),
        )
        .build();
        assert!(client.connect(None).await);

        // Make a request and expect the .bytes() call to fail
        let result = client.get::<()>("app/integer").bytes().await;

        assert!(
            result.is_err(),
            "Expected error from .bytes() due to server-side failure"
        );
    }
}
