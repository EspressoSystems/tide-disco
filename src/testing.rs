#![cfg(any(test, feature = "testing"))]

use crate::{http::Method, wait_for_server, Url, SERVER_STARTUP_RETRIES, SERVER_STARTUP_SLEEP_MS};
use async_compatibility_layer::logging::{setup_backtrace, setup_logging};
use async_tungstenite::{
    async_std::{connect_async, ConnectStream},
    tungstenite::{client::IntoClientRequest, http::header::*, Error as WsError},
    WebSocketStream,
};
use reqwest::RequestBuilder;
use std::time::Duration;

pub struct Client {
    inner: reqwest::Client,
    base_url: Url,
}

impl Client {
    pub async fn new(base_url: Url) -> Self {
        wait_for_server(&base_url, SERVER_STARTUP_RETRIES, SERVER_STARTUP_SLEEP_MS).await;
        Self {
            inner: reqwest::Client::builder()
                .timeout(Duration::from_secs(60))
                .build()
                .unwrap(),
            base_url,
        }
    }

    pub fn request(&self, method: Method, path: &str) -> RequestBuilder {
        let req_method: reqwest::Method = method.to_string().parse().unwrap();
        self.inner
            .request(req_method, self.base_url.join(path).unwrap())
    }

    pub fn get(&self, path: &str) -> RequestBuilder {
        self.request(Method::Get, path)
    }

    pub fn post(&self, path: &str) -> RequestBuilder {
        self.request(Method::Post, path)
    }
}

pub fn setup_test() {
    setup_logging();
    setup_backtrace();
}

pub async fn test_ws_client(url: Url) -> WebSocketStream<ConnectStream> {
    test_ws_client_with_headers(url, &[]).await
}

pub async fn test_ws_client_with_headers(
    mut url: Url,
    headers: &[(HeaderName, &str)],
) -> WebSocketStream<ConnectStream> {
    wait_for_server(&url, SERVER_STARTUP_RETRIES, SERVER_STARTUP_SLEEP_MS).await;
    url.set_scheme("ws").unwrap();

    // Follow redirects.
    loop {
        let mut req = url.clone().into_client_request().unwrap();
        for (name, value) in headers {
            req.headers_mut().insert(name, value.parse().unwrap());
        }

        match connect_async(req).await {
            Ok((conn, _)) => return conn,
            Err(WsError::Http(res)) if (301..=308).contains(&u16::from(res.status())) => {
                let location = res.headers()["location"].to_str().unwrap();
                tracing::info!(from = %url, to = %location, "WS handshake following redirect");
                url.set_path(location);
            }
            Err(err) => panic!("socket connection failed: {err}"),
        }
    }
}
