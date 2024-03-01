// Copyright (c) 2022 Espresso Systems (espressosys.com)
// This file is part of the tide-disco library.

// You should have received a copy of the MIT License
// along with the tide-disco library. If not, see <https://mit-license.org/>.

use crate::{
    api::ApiMetadata,
    healthcheck::HealthCheck,
    method::{Method, ReadState},
    metrics,
    request::{best_response_type, RequestError, RequestParam, RequestParamType, RequestParams},
    socket::{self, SocketError},
    Html, StatusCode,
};
use async_std::sync::Arc;
use async_trait::async_trait;
use derivative::Derivative;
use derive_more::From;
use futures::future::{BoxFuture, FutureExt};
use maud::{html, PreEscaped};
use serde::Serialize;
use snafu::{OptionExt, Snafu};
use std::borrow::Cow;
use std::collections::HashMap;
use std::convert::Infallible;
use std::fmt::{self, Display, Formatter};
use std::marker::PhantomData;
use std::str::FromStr;
use tide::{
    http::{
        self,
        content::Accept,
        mime::{self, Mime},
    },
    Body,
};
use tide_websockets::WebSocketConnection;
use versioned_binary_serialization::{BinarySerializer, Serializer};

/// An error returned by a route handler.
///
/// [RouteError] encapsulates application specific errors `E` returned by the user-installed handler
/// itself. It also includes errors in the route dispatching logic, such as failures to turn the
/// result of the user-installed handler into an HTTP response.
pub enum RouteError<E> {
    AppSpecific(E),
    Request(RequestError),
    UnsupportedContentType,
    Binary(anyhow::Error),
    Json(serde_json::Error),
    Tide(tide::Error),
    ExportMetrics(String),
    IncorrectMethod { expected: Method },
}

impl<E: Display> Display for RouteError<E> {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        match self {
            Self::AppSpecific(err) => write!(f, "{}", err),
            Self::Request(err) => write!(f, "{}", err),
            Self::UnsupportedContentType => write!(f, "requested content type is not supported"),
            Self::Binary(err) => write!(f, "error creating byte stream: {}", err),
            Self::Json(err) => write!(f, "error creating JSON response: {}", err),
            Self::Tide(err) => write!(f, "{}", err),
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
                StatusCode::BadRequest
            }
            _ => StatusCode::InternalServerError,
        }
    }

    pub fn map_app_specific<E2>(self, f: impl Fn(E) -> E2) -> RouteError<E2> {
        match self {
            RouteError::AppSpecific(e) => RouteError::AppSpecific(f(e)),
            RouteError::Request(e) => RouteError::Request(e),
            RouteError::UnsupportedContentType => RouteError::UnsupportedContentType,
            RouteError::Binary(err) => RouteError::Binary(err),
            RouteError::Json(err) => RouteError::Json(err),
            RouteError::Tide(err) => RouteError::Tide(err),
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

/// A route handler.
///
/// The [Handler] trait defines the interface required of route handlers -- they must be able to
/// take [RequestParams] and produce either a response or an appropriately typed error.
///
/// Implementations of this trait are provided for handler functions returning a serializable
/// response ([FnHandler]), for boxed handlers, and for the [MapErr] type which can be used to
/// transform the `Error` type of a [Handler]. This trait is usually used as
/// `Box<dyn Handler<State, Error>>`, in order to type-erase route-specific details such as the
/// return type of a handler function. The types which are preserved, `State` and `Error`, should be
/// the same for all handlers in an API module.
#[async_trait]
pub(crate) trait Handler<State, Error, const MAJOR: u16, const MINOR: u16>:
    'static + Send + Sync
{
    async fn handle(
        &self,
        req: RequestParams,
        state: &State,
    ) -> Result<tide::Response, RouteError<Error>>;
}

/// A [Handler] which delegates to an async function.
///
/// The function type `F` should be callable as
/// `async fn(RequestParams, &State) -> Result<R, Error>`. The [Handler] implementation will
/// automatically convert the result `R` to a [tide::Response] by serializing it, or the error
/// `Error` to a [RouteError] using [RouteError::AppSpecific]. Note that the format used for
/// serializing the response is flexible. This implementation will use the [Accept] header of the
/// incoming request to try to provide a format that the client expects. Supported formats are
/// `application/json` (using [serde_json]) and `application/octet-stream` (using [bincode]).
///
/// # Limitations
///
/// [Like many function parameters](crate#boxed-futures) in [tide_disco](crate), the handler
/// function is required to return a [BoxFuture].
#[derive(From)]
pub(crate) struct FnHandler<F>(F);

#[async_trait]
impl<F, T, State, Error, const MAJOR: u16, const MINOR: u16> Handler<State, Error, MAJOR, MINOR>
    for FnHandler<F>
where
    F: 'static + Send + Sync + Fn(RequestParams, &State) -> BoxFuture<'_, Result<T, Error>>,
    T: Serialize,
    State: 'static + Send + Sync,
{
    async fn handle(
        &self,
        req: RequestParams,
        state: &State,
    ) -> Result<tide::Response, RouteError<Error>> {
        let accept = req.accept()?;
        response_from_result::<_, _, MAJOR, MINOR>(&accept, (self.0)(req, state).await)
    }
}

pub(crate) fn response_from_result<T: Serialize, Error, const MAJOR: u16, const MINOR: u16>(
    accept: &Accept,
    res: Result<T, Error>,
) -> Result<tide::Response, RouteError<Error>> {
    res.map_err(RouteError::AppSpecific)
        .and_then(|res| respond_with::<_, _, MAJOR, MINOR>(accept, &res))
}

#[async_trait]
impl<
        H: ?Sized + Handler<State, Error, MAJOR, MINOR>,
        State: 'static + Send + Sync,
        Error,
        const MAJOR: u16,
        const MINOR: u16,
    > Handler<State, Error, MAJOR, MINOR> for Box<H>
{
    async fn handle(
        &self,
        req: RequestParams,
        state: &State,
    ) -> Result<tide::Response, RouteError<Error>> {
        (**self).handle(req, state).await
    }
}

enum RouteImplementation<State, Error, const MAJOR: u16, const MINOR: u16> {
    Http {
        method: http::Method,
        handler: Option<Box<dyn Handler<State, Error, MAJOR, MINOR>>>,
    },
    Socket {
        handler: Option<socket::Handler<State, Error>>,
    },
    Metrics {
        handler: Option<Box<dyn Handler<State, Error, MAJOR, MINOR>>>,
    },
}

impl<State, Error, const MAJOR: u16, const MINOR: u16>
    RouteImplementation<State, Error, MAJOR, MINOR>
{
    fn map_err<Error2>(
        self,
        f: impl 'static + Send + Sync + Fn(Error) -> Error2,
    ) -> RouteImplementation<State, Error2, MAJOR, MINOR>
    where
        State: 'static + Send + Sync,
        Error: 'static + Send + Sync,
        Error2: 'static,
    {
        match self {
            Self::Http { method, handler } => RouteImplementation::Http {
                method,
                handler: handler.map(|h| {
                    let h: Box<dyn Handler<State, Error2, MAJOR, MINOR>> =
                        Box::new(MapErr::<
                            Box<dyn Handler<State, Error, MAJOR, MINOR>>,
                            _,
                            Error,
                        >::new(h, f));
                    h
                }),
            },
            Self::Socket { handler } => RouteImplementation::Socket {
                handler: handler.map(|h| socket::map_err(h, f)),
            },
            Self::Metrics { handler } => RouteImplementation::Metrics {
                handler: handler.map(|h| {
                    let h: Box<dyn Handler<State, Error2, MAJOR, MINOR>> =
                        Box::new(MapErr::<
                            Box<dyn Handler<State, Error, MAJOR, MINOR>>,
                            _,
                            Error,
                        >::new(h, f));
                    h
                }),
            },
        }
    }
}

/// All the information we need to parse, typecheck, and dispatch a request.
///
/// A [Route] is a structured representation of a route specification from an `api.toml` API spec.
/// It can be parsed from a TOML specification, and it also includes an optional handler function
/// which the Rust server can register. Routes with no handler will use a default handler that
/// simply returns information about the route.
#[derive(Derivative)]
#[derivative(Debug(bound = ""))]
pub struct Route<State, Error, const MAJOR: u16, const MINOR: u16> {
    name: String,
    patterns: Vec<String>,
    params: Vec<RequestParam>,
    doc: String,
    meta: Arc<ApiMetadata>,
    #[derivative(Debug = "ignore")]
    handler: RouteImplementation<State, Error, MAJOR, MINOR>,
}

#[derive(Clone, Debug, Snafu)]
pub enum RouteParseError {
    MissingPathArray,
    PathElementError,
    InvalidTypeExpression,
    UnrecognizedType,
    MethodMustBeString,
    InvalidMethod,
    MissingPath,
    IncorrectPathType,
    IncorrectParamType,
    IncorrectDocType,
    RouteMustBeTable,
}

impl<State, Error, const MAJOR: u16, const MINOR: u16> Route<State, Error, MAJOR, MINOR> {
    /// Parse a [Route] from a TOML specification.
    ///
    /// The specification must be a table containing at least the following keys:
    /// * `PATH`: an array of patterns for this route
    /// * for each route parameter (route segment starting with `:`), a key with the same name as
    ///   the parameter (including the `:`) whose value is the type of the parameter
    ///
    /// In addition, the following optional keys may be specified:
    /// * `METHOD`: the method to use to dispatch the route (default `GET`)
    /// * `DOC`: Markdown description of the route
    pub fn new(
        name: String,
        spec: &toml::Value,
        meta: Arc<ApiMetadata>,
    ) -> Result<Self, RouteParseError> {
        let paths: Vec<String> = spec["PATH"]
            .as_array()
            .ok_or(RouteParseError::MissingPathArray)?
            .iter()
            .map(|v| {
                v.as_str()
                    .ok_or(RouteParseError::PathElementError)
                    .unwrap()
                    .to_string()
            })
            .collect();
        let mut params = HashMap::<String, RequestParam>::new();
        for path in paths.iter() {
            for seg in path.split('/') {
                if let Some(name) = seg.strip_prefix(':') {
                    let param_type = RequestParamType::from_str(
                        spec[seg]
                            .as_str()
                            .ok_or(RouteParseError::InvalidTypeExpression)?,
                    )
                    .map_err(|_| RouteParseError::UnrecognizedType)?;
                    params.insert(
                        seg.to_string(),
                        RequestParam {
                            name: name.to_string(),
                            param_type,
                        },
                    );
                }
            }
        }
        let method = match spec.get("METHOD") {
            Some(val) => val
                .as_str()
                .context(MethodMustBeStringSnafu)?
                .parse()
                .map_err(|_| RouteParseError::InvalidMethod)?,
            None => Method::get(),
        };
        let handler = match method {
            Method::Http(method) => RouteImplementation::Http {
                method,
                handler: None,
            },
            Method::Socket => RouteImplementation::Socket { handler: None },
            Method::Metrics => RouteImplementation::Metrics { handler: None },
        };
        Ok(Route {
            name,
            patterns: match spec.get("PATH").context(MissingPathSnafu)? {
                toml::Value::String(s) => vec![s.clone()],
                toml::Value::Array(paths) => paths
                    .iter()
                    .map(|path| Ok(path.as_str().context(IncorrectPathTypeSnafu)?.to_string()))
                    .collect::<Result<_, _>>()?,
                _ => return Err(RouteParseError::IncorrectPathType),
            },
            params: params.into_values().collect(),
            handler,
            doc: match spec.get("DOC") {
                Some(doc) => markdown::to_html(doc.as_str().context(IncorrectDocTypeSnafu)?),
                None => String::new(),
            },
            meta,
        })
    }

    /// The name of the route.
    ///
    /// This is the name used to identify the route when binding a handler. It is also the first
    /// segment of all of the URL patterns for this route.
    pub fn name(&self) -> String {
        self.name.clone()
    }

    /// Iterate over route patterns.
    pub fn patterns(&self) -> impl Iterator<Item = &String> {
        self.patterns.iter()
    }

    /// The HTTP method of the route.
    pub fn method(&self) -> Method {
        match &self.handler {
            RouteImplementation::Http { method, .. } => (*method).into(),
            RouteImplementation::Socket { .. } => Method::socket(),
            RouteImplementation::Metrics { .. } => Method::metrics(),
        }
    }

    /// Whether a non-default handler has been bound to this route.
    pub fn has_handler(&self) -> bool {
        match &self.handler {
            RouteImplementation::Http { handler, .. } => handler.is_some(),
            RouteImplementation::Socket { handler, .. } => handler.is_some(),
            RouteImplementation::Metrics { handler, .. } => handler.is_some(),
        }
    }

    /// Get all formal parameters.
    pub fn params(&self) -> &[RequestParam] {
        &self.params
    }

    /// Create a new route with a modified error type.
    pub fn map_err<Error2>(
        self,
        f: impl 'static + Send + Sync + Fn(Error) -> Error2,
    ) -> Route<State, Error2, MAJOR, MINOR>
    where
        State: 'static + Send + Sync,
        Error: 'static + Send + Sync,
        Error2: 'static,
    {
        Route {
            handler: self.handler.map_err(f),
            name: self.name,
            patterns: self.patterns,
            params: self.params,
            doc: self.doc,
            meta: self.meta,
        }
    }

    /// Compose an HTML fragment documenting all the variations on this route.
    pub fn documentation(&self) -> Html {
        html! {
            (PreEscaped(self.meta.heading_entry
                .replace("{{METHOD}}", &self.method().to_string())
                .replace("{{NAME}}", &self.name())))
            (PreEscaped(&self.meta.heading_routes))
            @for path in self.patterns() {
                (PreEscaped(self.meta.route_path.replace("{{PATH}}", &format!("/{}/{}", self.meta.name, path))))
            }
            (PreEscaped(&self.meta.heading_parameters))
            (PreEscaped(&self.meta.parameter_table_open))
            @for param in self.params() {
                (PreEscaped(self.meta.parameter_row
                    .replace("{{NAME}}", &param.name)
                    .replace("{{TYPE}}", &param.param_type.to_string())))
            }
            @if self.params().is_empty() {
                (PreEscaped(&self.meta.parameter_none))
            }
            (PreEscaped(&self.meta.parameter_table_close))
            (PreEscaped(&self.meta.heading_description))
            (PreEscaped(&self.doc))
        }
    }
}

impl<State, Error, const MAJOR: u16, const MINOR: u16> Route<State, Error, MAJOR, MINOR> {
    pub(crate) fn set_handler(
        &mut self,
        h: impl Handler<State, Error, MAJOR, MINOR>,
    ) -> Result<(), RouteError<Error>> {
        match &mut self.handler {
            RouteImplementation::Http { handler, .. } => {
                *handler = Some(Box::new(h));
                Ok(())
            }
            _ => Err(RouteError::IncorrectMethod {
                expected: self.method(),
            }),
        }
    }

    pub(crate) fn set_fn_handler<F, T>(&mut self, handler: F) -> Result<(), RouteError<Error>>
    where
        F: 'static + Send + Sync + Fn(RequestParams, &State) -> BoxFuture<'_, Result<T, Error>>,
        T: Serialize,
        State: 'static + Send + Sync,
    {
        self.set_handler(FnHandler::from(handler))
    }

    pub(crate) fn set_socket_handler(
        &mut self,
        h: socket::Handler<State, Error>,
    ) -> Result<(), RouteError<Error>> {
        match &mut self.handler {
            RouteImplementation::Socket { handler, .. } => {
                *handler = Some(h);
                Ok(())
            }
            _ => Err(RouteError::IncorrectMethod {
                expected: self.method(),
            }),
        }
    }

    pub(crate) fn set_metrics_handler<F, T>(&mut self, h: F) -> Result<(), RouteError<Error>>
    where
        F: 'static
            + Send
            + Sync
            + Fn(RequestParams, &State::State) -> BoxFuture<Result<Cow<T>, Error>>,
        T: 'static + Clone + metrics::Metrics,
        State: 'static + Send + Sync + ReadState,
        Error: 'static,
    {
        match &mut self.handler {
            RouteImplementation::Metrics { handler, .. } => {
                *handler = Some(Box::new(metrics::Handler::from(h)));
                Ok(())
            }
            _ => Err(RouteError::IncorrectMethod {
                expected: self.method(),
            }),
        }
    }

    /// Print documentation about the route, to aid the developer when the route is not yet
    /// implemented.
    pub(crate) fn default_handler(&self) -> Result<tide::Response, RouteError<Error>> {
        Ok(self.documentation().into())
    }

    pub(crate) async fn handle_socket(
        &self,
        req: RequestParams,
        conn: WebSocketConnection,
        state: &State,
    ) -> Result<(), SocketError<Error>> {
        match &self.handler {
            RouteImplementation::Socket { handler, .. } => match handler {
                Some(handler) => handler(req, conn, state).await,
                // If there is no socket handler registered, the top-level app should register the
                // route in such a way that the socket upgrade request is rejected and the route
                // instead displays a simple HTML page with the documentation for the route. That
                // means that we should not get here.
                None => unreachable!(),
            },
            _ => Err(SocketError::IncorrectMethod {
                expected: self.method(),
                actual: req.method(),
            }),
        }
    }
}

#[async_trait]
impl<State, Error, const MAJOR: u16, const MINOR: u16> Handler<State, Error, MAJOR, MINOR>
    for Route<State, Error, MAJOR, MINOR>
where
    Error: 'static,
    State: 'static + Send + Sync,
{
    async fn handle(
        &self,
        req: RequestParams,
        state: &State,
    ) -> Result<tide::Response, RouteError<Error>> {
        match &self.handler {
            RouteImplementation::Http { handler, .. }
            | RouteImplementation::Metrics { handler, .. } => match handler {
                Some(handler) => handler.handle(req, state).await,
                None => self.default_handler(),
            },
            RouteImplementation::Socket { .. } => Err(RouteError::IncorrectMethod {
                expected: self.method(),
            }),
        }
    }
}

pub struct MapErr<H, F, E> {
    handler: H,
    map: F,
    _phantom: PhantomData<E>,
}

impl<H, F, E> MapErr<H, F, E> {
    fn new(handler: H, map: F) -> Self {
        Self {
            handler,
            map,
            _phantom: Default::default(),
        }
    }
}

#[async_trait]
impl<H, F, State, Error1, Error2, const MAJOR: u16, const MINOR: u16>
    Handler<State, Error2, MAJOR, MINOR> for MapErr<H, F, Error1>
where
    H: Handler<State, Error1, MAJOR, MINOR>,
    F: 'static + Send + Sync + Fn(Error1) -> Error2,
    State: 'static + Send + Sync,
    Error1: 'static + Send + Sync,
    Error2: 'static,
{
    async fn handle(
        &self,
        req: RequestParams,
        state: &State,
    ) -> Result<tide::Response, RouteError<Error2>> {
        self.handler
            .handle(req, state)
            .await
            .map_err(|err| err.map_app_specific(&self.map))
    }
}

/// A special kind of route handler representing a health check.
///
/// In order to type-erase the implementation of `HealthCheck` that the handler actually returns,
/// we store the healthcheck handler in a wrapper that converts the `HealthCheck` to a
/// `tide::Response`. The status code of the response will be the status code of the `HealthCheck`
/// implementation returned by the underlying handler.
pub(crate) type HealthCheckHandler<State> =
    Box<dyn 'static + Send + Sync + Fn(RequestParams, &State) -> BoxFuture<'_, tide::Response>>;

pub(crate) fn health_check_response<H: HealthCheck, const MAJOR: u16, const MINOR: u16>(
    accept: &Accept,
    health: H,
) -> tide::Response {
    let status = health.status();
    let (body, content_type) = response_body::<H, Infallible, MAJOR, MINOR>(accept, health)
        .unwrap_or_else(|err| {
            let msg = format!(
                "health status was {}, but there was an error generating the response: {}",
                status, err
            );
            (msg.into(), mime::PLAIN)
        });
    tide::Response::builder(status)
        .content_type(content_type)
        .body(body)
        .build()
}

/// A type-erasing wrapper for healthcheck handlers.
///
/// Given a handler, this function can be used to derive a new, type-erased [HealthCheckHandler]
/// that takes only [RequestParams] and returns a generic [tide::Response].
pub(crate) fn health_check_handler<State, H, const MAJOR: u16, const MINOR: u16>(
    handler: impl 'static + Send + Sync + Fn(&State) -> BoxFuture<H>,
) -> HealthCheckHandler<State>
where
    State: 'static + Send + Sync,
    H: 'static + HealthCheck,
{
    Box::new(move |req, state| {
        let accept = req.accept().unwrap_or_else(|_| {
            // The healthcheck endpoint is not allowed to fail, so just use the default content
            // type if we can't parse the Accept header.
            let mut accept = Accept::new();
            accept.set_wildcard(true);
            accept
        });
        let future = handler(state);
        async move {
            let health = future.await;
            health_check_response::<_, MAJOR, MINOR>(&accept, health)
        }
        .boxed()
    })
}

pub(crate) fn response_body<T: Serialize, E, const MAJOR: u16, const MINOR: u16>(
    accept: &Accept,
    body: T,
) -> Result<(Body, Mime), RouteError<E>> {
    let ty = best_response_type(accept, &[mime::JSON, mime::BYTE_STREAM])?;
    if ty == mime::BYTE_STREAM {
        let bytes = Serializer::<MAJOR, MINOR>::serialize(&body).map_err(RouteError::Binary)?;
        Ok((bytes.into(), mime::BYTE_STREAM))
    } else if ty == mime::JSON {
        let json = serde_json::to_string(&body).map_err(RouteError::Json)?;
        Ok((json.into(), mime::JSON))
    } else {
        unreachable!()
    }
}

pub(crate) fn respond_with<T: Serialize, E, const MAJOR: u16, const MINOR: u16>(
    accept: &Accept,
    body: T,
) -> Result<tide::Response, RouteError<E>> {
    let (body, content_type) = response_body::<_, _, MAJOR, MINOR>(accept, body)?;
    Ok(tide::Response::builder(StatusCode::Ok)
        .body(body)
        .content_type(content_type)
        .build())
}
