// Copyright (c) 2022 Espresso Systems (espressosys.com)
// This file is part of the surf-disco library.

// You should have received a copy of the MIT License
// along with the surf-disco library. If not, see <https://mit-license.org/>.

use crate::{
    http, request::reqwest_error_msg, Error, Method, Request, SocketRequest, StatusCode, Url,
};
use async_std::task::sleep;
use async_tungstenite::tungstenite::protocol::WebSocketConfig;
use derivative::Derivative;
use serde::de::DeserializeOwned;
use std::time::{Duration, Instant};
use vbs::version::StaticVersionType;

pub use tide_disco::healthcheck::{HealthCheck, HealthStatus};

/// Content types supported by Tide Disco.
#[derive(Clone, Copy, Debug)]
pub enum ContentType {
    Json,
    Binary,
}

impl From<ContentType> for http::Mime {
    fn from(c: ContentType) -> http::Mime {
        match c {
            ContentType::Json => http::mime::JSON,
            ContentType::Binary => http::mime::BYTE_STREAM,
        }
    }
}

/// A client of a Tide Disco application.
#[derive(Derivative)]
#[derivative(Clone(bound = ""), Debug(bound = ""))]
pub struct Client<E, VER: StaticVersionType> {
    inner: reqwest::Client,
    base_url: Url,
    accept: ContentType,
    _marker: std::marker::PhantomData<fn(E, VER) -> ()>,
}

impl<E: Error, VER: StaticVersionType> Client<E, VER> {
    /// Create a client and connect to the Tide Disco server at `base_url`.
    pub fn new(base_url: Url) -> Self {
        Self::builder(base_url).build()
    }

    /// Create a client with customization.
    pub fn builder(base_url: Url) -> ClientBuilder<E, VER> {
        ClientBuilder::<E, VER>::new(base_url)
    }

    /// Connect to the server, retrying if the server is not running.
    ///
    /// It is not necessary to call this function when creating a new client. The client will
    /// automatically connect when a request is made, if the server is available. However, this can
    /// be useful to wait for the server to come up, if the server may be offline when the client is
    /// created.
    ///
    /// This function will make an HTTP `GET` request to the server's `/healthcheck` endpoint, to
    /// test if the server is available. If this request succeeds, [connect](Self::connect) returns
    /// `true`. Otherwise, the client will continue retrying `/healthcheck` requests until `timeout`
    /// has elapsed (or forever, if `timeout` is `None`). If the timeout expires before a
    /// `/healthcheck` request succeeds, [connect](Self::connect) will return `false`.
    pub async fn connect(&self, timeout: Option<Duration>) -> bool {
        let timeout = timeout.map(|d| Instant::now() + d);
        while timeout.map(|t| Instant::now() < t).unwrap_or(true) {
            match self
                .inner
                .get(self.base_url.join("/healthcheck").unwrap())
                .send()
                .await
            {
                Ok(res) if res.status() == StatusCode::OK => return true,
                Ok(res) => {
                    tracing::info!(
                        url = %self.base_url,
                        status = %res.status(),
                        "waiting for server to become ready",
                    );
                }
                Err(err) => {
                    tracing::info!(
                        url = %self.base_url,
                        err = reqwest_error_msg(err),
                        "waiting for server to become ready",
                    );
                }
            }
            sleep(Duration::from_secs(10)).await;
        }
        false
    }

    /// Connect to the server, retrying until the server is `healthy`.
    ///
    /// This function is similar to [connect](Self::connect). It will make requests to the
    /// `/healthcheck` endpoint until a request succeeds. However, it will then continue retrying
    /// until the response from `/healthcheck` satisfies the `healthy` predicate.
    ///
    /// On success, returns the response from `/healthcheck`. On timeout, returns `None`.
    pub async fn wait_for_health<H: DeserializeOwned + HealthCheck>(
        &self,
        healthy: impl Fn(&H) -> bool,
        timeout: Option<Duration>,
    ) -> Option<H> {
        let timeout = timeout.map(|d| Instant::now() + d);
        while timeout.map(|t| Instant::now() < t).unwrap_or(true) {
            match self.healthcheck::<H>().await {
                Ok(health) if healthy(&health) => return Some(health),
                _ => sleep(Duration::from_secs(10)).await,
            }
        }
        None
    }

    /// Build an HTTP `GET` request.
    pub fn get<T: DeserializeOwned>(&self, route: &str) -> Request<T, E, VER> {
        self.request(Method::Get, route)
    }

    /// Build an HTTP `POST` request.
    pub fn post<T: DeserializeOwned>(&self, route: &str) -> Request<T, E, VER> {
        self.request(Method::Post, route)
    }

    /// Query the server's healthcheck endpoint.
    pub async fn healthcheck<H: DeserializeOwned + HealthCheck>(&self) -> Result<H, E> {
        self.get("healthcheck").send().await
    }

    /// Build an HTTP request with the specified method.
    pub fn request<T: DeserializeOwned>(&self, method: Method, route: &str) -> Request<T, E, VER> {
        Request::from(self.inner.request(
            method.to_string().parse().unwrap(),
            self.base_url.join(route).unwrap(),
        ))
        .header("Accept", http::Mime::from(self.accept).to_string())
    }

    /// Build a streaming connection request.
    ///
    /// # Panics
    ///
    /// This will panic if a malformed URL is passed.
    pub fn socket(&self, route: &str) -> SocketRequest<E, VER> {
        SocketRequest::new(self.base_url.join(route).unwrap(), self.accept, None)
            .header("Accept", http::Mime::from(self.accept).to_string())
    }

    /// Build a streaming connection request using a custom [`WebSocketConfig`].
    ///
    /// # Panics
    ///
    /// This will panic if a malformed URL is passed.
    pub fn socket_with_config(
        &self,
        route: &str,
        config: WebSocketConfig,
    ) -> SocketRequest<E, VER> {
        SocketRequest::new(
            self.base_url.join(route).unwrap(),
            self.accept,
            Some(config),
        )
        .header("Accept", http::Mime::from(self.accept).to_string())
    }

    /// Create a client for a sub-module of the connected application.
    pub fn module<ModError: Error>(
        &self,
        prefix: &str,
    ) -> Result<Client<ModError, VER>, http::url::ParseError> {
        Ok(Client {
            inner: self.inner.clone(),
            base_url: self.base_url.join(prefix)?,
            accept: self.accept,
            _marker: Default::default(),
        })
    }

    pub fn base_url(&self) -> Url {
        self.base_url.clone()
    }
}

/// Interface to specify optional configuration values before creating a [Client].
pub struct ClientBuilder<E: Error, VER: StaticVersionType> {
    inner: reqwest::ClientBuilder,
    accept: ContentType,
    base_url: Url,
    timeout: Option<Duration>,
    _marker: std::marker::PhantomData<fn(E, VER) -> ()>,
}

impl<E: Error, VER: StaticVersionType> ClientBuilder<E, VER> {
    fn new(mut base_url: Url) -> Self {
        // If the path part of `base_url` does not end in `/`, `join` will treat it as a filename
        // and remove it, which is never what we want: `base_url` is _always_ a directory-like path.
        // To avoid the annoyance of having every caller add a trailing slash if necessary, we will
        // add a trailing slash here if there isn't one already.
        if !base_url.path().ends_with('/') {
            base_url.set_path(&format!("{}/", base_url.path()));
        }
        Self {
            inner: reqwest::Client::builder(),
            accept: ContentType::Binary,
            base_url,
            timeout: Some(Duration::from_secs(60)),
            _marker: Default::default(),
        }
    }

    /// Set connection timeout duration.
    ///
    /// Passing `None` will remove the timeout.
    ///
    /// Default: `Some(Duration::from_secs(60))`.
    pub fn set_timeout(mut self, timeout: Option<Duration>) -> Self {
        self.timeout = timeout;
        self
    }

    /// Set the content type used for responses.
    pub fn content_type(mut self, content_type: ContentType) -> Self {
        self.accept = content_type;
        self
    }

    /// Create a [Client] with the settings specified in this builder.
    pub fn build(self) -> Client<E, VER> {
        let mut builder = self.inner;

        if let Some(timeout) = self.timeout {
            builder = builder.timeout(timeout);
        }

        Client {
            inner: builder.build().unwrap(),
            base_url: self.base_url,
            accept: self.accept,
            _marker: Default::default(),
        }
    }
}

impl<E: Error, VER: StaticVersionType> From<ClientBuilder<E, VER>> for Client<E, VER> {
    fn from(builder: ClientBuilder<E, VER>) -> Self {
        builder.build()
    }
}

#[cfg(test)]
mod test {
    use crate::socket::Connection;

    use super::*;
    use async_compatibility_layer::logging::{setup_backtrace, setup_logging};
    use async_std::{sync::RwLock, task::spawn};
    use futures::{stream::iter, FutureExt, SinkExt, StreamExt};
    use portpicker::pick_unused_port;
    use serde::{Deserialize, Serialize};
    use tide_disco::{error::ServerError, App};
    use toml::toml;
    use vbs::version::StaticVersion;
    type Ver01 = StaticVersion<0, 1>;
    const VER_0_1: Ver01 = StaticVersion {};

    async fn test_basic_http_client(accept: ContentType) {
        setup_logging();
        setup_backtrace();

        // Set up a simple Tide Disco app as an example.
        let mut app: App<(), ServerError> = App::with_state(());
        let api = toml! {
            [route.get]
            PATH = ["/get"]
            METHOD = "GET"

            [route.post]
            PATH = ["/post"]
            METHOD = "POST"
        };
        app.module::<ServerError, Ver01>("mod", api)
            .unwrap()
            .get("get", |_req, _state| async move { Ok("response") }.boxed())
            .unwrap()
            .post("post", |req, _state| {
                async move {
                    if req.body_auto::<String, _>(VER_0_1).unwrap() == "body" {
                        Ok("response")
                    } else {
                        Err(ServerError::catch_all(
                            StatusCode::BAD_REQUEST,
                            "invalid body".into(),
                        ))
                    }
                }
                .boxed()
            })
            .unwrap();
        let port = pick_unused_port().unwrap();
        spawn(app.serve(format!("0.0.0.0:{}", port), VER_0_1));

        // Connect a client.
        let client = Client::<ServerError, Ver01>::builder(
            format!("http://localhost:{}", port).parse().unwrap(),
        )
        .content_type(accept)
        .build();
        assert!(client.connect(None).await);

        // Test a couple of basic requests.
        assert_eq!(
            client.get::<String>("mod/get").send().await.unwrap(),
            "response"
        );
        assert_eq!(
            client
                .post::<String>("mod/post")
                .body_json(&"body".to_string())
                .unwrap()
                .send()
                .await
                .unwrap(),
            "response"
        );

        // Test an error response.
        let err = client
            .post::<String>("mod/post")
            .body_json(&"bad".to_string())
            .unwrap()
            .send()
            .await
            .unwrap_err();
        if err.status != StatusCode::BAD_REQUEST || err.message != "invalid body" {
            panic!("unexpected error {}", err);
        }
    }

    #[async_std::test]
    async fn test_basic_http_client_json() {
        test_basic_http_client(ContentType::Json).await
    }

    #[async_std::test]
    async fn test_basic_http_client_binary() {
        test_basic_http_client(ContentType::Binary).await
    }

    async fn test_streaming_client(accept: ContentType) {
        setup_logging();
        setup_backtrace();

        // Set up a simple Tide Disco app as an example.
        let mut app: App<(), ServerError> = App::with_state(());
        let api = toml! {
            [route.echo]
            PATH = ["/echo"]
            METHOD = "SOCKET"

            [route.naturals]
            PATH = ["/naturals/:max"]
            METHOD = "SOCKET"
            ":max" = "Integer"
        };
        app.module::<ServerError, Ver01>("mod", api)
            .unwrap()
            .socket::<_, String, String>("echo", |_req, mut conn, _state| {
                async move {
                    while let Some(Ok(msg)) = conn.next().await {
                        conn.send(&msg).await.unwrap();
                    }
                    Ok(())
                }
                .boxed()
            })
            .unwrap()
            .stream("naturals", |req, _state| {
                iter(0u64..req.integer_param("max").unwrap())
                    .map(Ok)
                    .boxed()
            })
            .unwrap();
        let port = pick_unused_port().unwrap();
        spawn(app.serve(format!("0.0.0.0:{}", port), VER_0_1));

        // Connect a client.
        let client: Client<ServerError, _> =
            Client::builder(format!("http://localhost:{}", port).parse().unwrap())
                .content_type(accept)
                .build();
        assert!(client.connect(None).await);

        // Test a bidirectional endpoint.
        let mut conn: Connection<_, _, _, Ver01> = client
            .socket("mod/echo")
            .connect::<String, String>()
            .await
            .unwrap();
        conn.send(&"foo".into()).await.unwrap();
        assert_eq!(conn.next().await.unwrap().unwrap(), "foo");
        conn.send(&"bar".into()).await.unwrap();
        assert_eq!(conn.next().await.unwrap().unwrap(), "bar");

        // Test a streaming endpoint.
        assert_eq!(
            client
                .socket("mod/naturals/10")
                .subscribe::<u64>()
                .await
                .unwrap()
                .collect::<Vec<_>>()
                .await,
            (0..10).map(Ok).collect::<Vec<_>>()
        );
    }

    #[async_std::test]
    async fn test_streaming_client_json() {
        test_streaming_client(ContentType::Json).await
    }

    #[async_std::test]
    async fn test_streaming_client_binary() {
        test_streaming_client(ContentType::Binary).await
    }

    #[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
    enum HealthCheck {
        Ready,
        Initializing,
    }

    impl super::HealthCheck for HealthCheck {
        fn status(&self) -> StatusCode {
            StatusCode::OK
        }
    }

    #[async_std::test]
    async fn test_healthcheck() {
        setup_logging();
        setup_backtrace();

        // Set up a simple Tide Disco app as an example.
        let mut app: App<_, ServerError> = App::with_state(RwLock::new(HealthCheck::Initializing));
        let api = toml! {
            [route.init]
            PATH = ["/init"]
            METHOD = "POST"
        };
        app.module::<ServerError, Ver01>("mod", api)
            .unwrap()
            .with_health_check(|state| async move { *state.read().await }.boxed())
            .post("init", |_, state| {
                async move {
                    *state = HealthCheck::Ready;
                    Ok(())
                }
                .boxed()
            })
            .unwrap();
        let port = pick_unused_port().unwrap();
        spawn(app.serve(format!("0.0.0.0:{}", port), VER_0_1));

        // Connect a client.
        let client = Client::<ServerError, Ver01>::new(
            format!("http://localhost:{}/mod", port).parse().unwrap(),
        );
        assert!(client.connect(None).await);
        assert_eq!(
            HealthCheck::Initializing,
            client.healthcheck().await.unwrap()
        );

        // Waiting for [HealthCheck::Ready] should time out.
        assert_eq!(
            client
                .wait_for_health::<HealthCheck>(
                    |h| *h == HealthCheck::Ready,
                    Some(Duration::from_secs(1))
                )
                .await,
            None
        );

        // Initialize the service.
        client.post::<()>("init").send().await.unwrap();

        // Now waiting for [HealthCheck::Ready] should succeed.
        assert_eq!(
            client
                .wait_for_health::<HealthCheck>(|h| *h == HealthCheck::Ready, None)
                .await,
            Some(HealthCheck::Ready)
        );
        assert_eq!(HealthCheck::Ready, client.healthcheck().await.unwrap());
    }

    #[test]
    fn test_builder() {
        let client =
            Client::<ServerError, Ver01>::builder("http://www.example.com".parse().unwrap())
                .set_timeout(None)
                .build();
        assert_eq!(client.base_url, "http://www.example.com".parse().unwrap());
    }
}
