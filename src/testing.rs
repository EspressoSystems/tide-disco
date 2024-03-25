#![cfg(any(test, feature = "testing"))]

use crate::{wait_for_server, Url, SERVER_STARTUP_RETRIES, SERVER_STARTUP_SLEEP_MS};
use async_compatibility_layer::logging::{setup_backtrace, setup_logging};
use async_tungstenite::{
    async_std::{connect_async, ConnectStream},
    tungstenite::{client::IntoClientRequest, http::header::*, Error as WsError},
    WebSocketStream,
};
use surf::{middleware::Redirect, Client, Config};

pub fn setup_test() {
    setup_logging();
    setup_backtrace();
}

pub async fn test_client(url: Url) -> surf::Client {
    wait_for_server(&url, SERVER_STARTUP_RETRIES, SERVER_STARTUP_SLEEP_MS).await;
    Client::try_from(Config::new().set_base_url(url))
        .unwrap()
        .with(Redirect::default())
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
            Err(WsError::Http(res)) if res.status() == 302 => {
                let location = res.headers()["location"].to_str().unwrap();
                tracing::info!(from = %url, to = %location, "WS handshake following redirect");
                url.set_path(location);
            }
            Err(err) => panic!("socket connection failed: {err}"),
        }
    }
}
