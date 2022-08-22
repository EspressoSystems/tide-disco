use crate::{
    api::ApiMetadata,
    healthcheck::HealthCheck,
    method::Method,
    request::{best_response_type, RequestError, RequestParam, RequestParamType, RequestParams},
    socket::{self, SocketError},
    Html,
};
use async_trait::async_trait;
use derive_more::From;
use futures::future::{BoxFuture, Future, FutureExt};
use maud::{html, PreEscaped};
use serde::Serialize;
use snafu::{OptionExt, Snafu};
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
        StatusCode,
    },
    Body,
};
use tide_websockets::WebSocketConnection;

/// An error returned by a route handler.
///
/// [RouteError] encapsulates application specific errors `E` returned by the user-installed handler
/// itself. It also includes errors in the route dispatching logic, such as failures to turn the
/// result of the user-installed handler into an HTTP response.
pub enum RouteError<E> {
    AppSpecific(E),
    Request(RequestError),
    UnsupportedContentType,
    Bincode(bincode::Error),
    Json(serde_json::Error),
    Tide(tide::Error),
    IncorrectMethod { expected: Method },
}

impl<E: Display> Display for RouteError<E> {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        match self {
            Self::AppSpecific(err) => write!(f, "{}", err),
            Self::Request(err) => write!(f, "{}", err),
            Self::UnsupportedContentType => write!(f, "requested content type is not supported"),
            Self::Bincode(err) => write!(f, "error creating byte stream: {}", err),
            Self::Json(err) => write!(f, "error creating JSON response: {}", err),
            Self::Tide(err) => write!(f, "{}", err),
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
            RouteError::Bincode(err) => RouteError::Bincode(err),
            RouteError::Json(err) => RouteError::Json(err),
            RouteError::Tide(err) => RouteError::Tide(err),
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
pub(crate) trait Handler<State, Error>: 'static + Send + Sync {
    async fn handle(
        &self,
        req: RequestParams,
        state: &State,
    ) -> Result<tide::Response, RouteError<Error>>;
}

/// A [Handler] which delegates to an async function.
///
/// The function type `F` should be callable as
/// `async fn(RequestParams<State>) -> Result<R, Error>`. The [Handler] implementation will
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
impl<F, T, State, Error> Handler<State, Error> for FnHandler<F>
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
        response_from_result(&accept, (self.0)(req, state).await)
    }
}

pub(crate) fn response_from_result<T: Serialize, Error>(
    accept: &Accept,
    res: Result<T, Error>,
) -> Result<tide::Response, RouteError<Error>> {
    res.map_err(RouteError::AppSpecific)
        .and_then(|res| respond_with(accept, &res))
}

#[async_trait]
impl<H: ?Sized + Handler<State, Error>, State: 'static + Send + Sync, Error> Handler<State, Error>
    for Box<H>
{
    async fn handle(
        &self,
        req: RequestParams,
        state: &State,
    ) -> Result<tide::Response, RouteError<Error>> {
        (**self).handle(req, state).await
    }
}

enum RouteImplementation<State, Error> {
    Http {
        method: http::Method,
        handler: Option<Box<dyn Handler<State, Error>>>,
    },
    Socket {
        handler: Option<socket::Handler<State, Error>>,
    },
}

impl<State, Error> RouteImplementation<State, Error> {
    fn map_err<Error2>(
        self,
        f: impl 'static + Send + Sync + Fn(Error) -> Error2,
    ) -> RouteImplementation<State, Error2>
    where
        State: 'static + Send + Sync,
        Error: 'static + Send + Sync,
        Error2: 'static,
    {
        match self {
            Self::Http { method, handler } => RouteImplementation::Http {
                method,
                handler: handler.map(|h| {
                    let h: Box<dyn Handler<State, Error2>> =
                        Box::new(MapErr::<Box<dyn Handler<State, Error>>, _, Error>::new(
                            h, f,
                        ));
                    h
                }),
            },
            Self::Socket { handler } => RouteImplementation::Socket {
                handler: handler.map(|h| socket::map_err(h, f)),
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
pub struct Route<State, Error> {
    name: String,
    patterns: Vec<String>,
    params: Vec<RequestParam>,
    doc: String,
    handler: RouteImplementation<State, Error>,
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

impl<State, Error> Route<State, Error> {
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
    pub fn new(name: String, spec: &toml::Value) -> Result<Self, RouteParseError> {
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
        }
    }

    /// Whether a non-default handler has been bound to this route.
    pub fn has_handler(&self) -> bool {
        match &self.handler {
            RouteImplementation::Http { handler, .. } => handler.is_some(),
            RouteImplementation::Socket { handler, .. } => handler.is_some(),
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
    ) -> Route<State, Error2>
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
        }
    }

    /// Compose an HTML fragment documenting all the variations on this route.
    pub fn documentation(&self, meta: &ApiMetadata) -> Html {
        html! {
            (PreEscaped(meta.heading_entry
                .replace("{{METHOD}}", &self.method().to_string())
                .replace("{{NAME}}", &self.name())))
            (PreEscaped(&meta.heading_routes))
            @for path in self.patterns() {
                (PreEscaped(meta.route_path.replace("{{PATH}}", &format!("/{}/{}", meta.name, path))))
            }
            (PreEscaped(&meta.heading_parameters))
            (PreEscaped(&meta.parameter_table_open))
            @for param in self.params() {
                (PreEscaped(meta.parameter_row
                    .replace("{{NAME}}", &param.name)
                    .replace("{{TYPE}}", &param.param_type.to_string())))
            }
            @if self.params().is_empty() {
                (PreEscaped(&meta.parameter_none))
            }
            (PreEscaped(&meta.parameter_table_close))
            (PreEscaped(&meta.heading_description))
            (PreEscaped(&self.doc))
        }
    }
}

impl<State, Error> Route<State, Error> {
    pub(crate) fn set_handler(
        &mut self,
        h: impl Handler<State, Error>,
    ) -> Result<(), RouteError<Error>> {
        match &mut self.handler {
            RouteImplementation::Http { handler, .. } => {
                *handler = Some(Box::new(h));
                Ok(())
            }
            RouteImplementation::Socket { .. } => Err(RouteError::IncorrectMethod {
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
            RouteImplementation::Http { .. } => Err(RouteError::IncorrectMethod {
                expected: self.method(),
            }),
        }
    }

    /// Print documentation about the route, to aid the developer when the route is not yet
    /// implemented.
    pub(crate) fn default_handler(
        &self,
        _req: RequestParams,
        _state: &State,
    ) -> Result<tide::Response, RouteError<Error>> {
        unimplemented!()
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
            RouteImplementation::Http { .. } => Err(SocketError::IncorrectMethod {
                expected: self.method(),
                actual: req.method(),
            }),
        }
    }
}

#[async_trait]
impl<State, Error> Handler<State, Error> for Route<State, Error>
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
            RouteImplementation::Http { handler, .. } => match handler {
                Some(handler) => handler.handle(req, state).await,
                None => self.default_handler(req, state),
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
impl<H, F, State, Error1, Error2> Handler<State, Error2> for MapErr<H, F, Error1>
where
    H: Handler<State, Error1>,
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

pub(crate) fn health_check_response<H: HealthCheck>(accept: &Accept, health: H) -> tide::Response {
    let status = health.status();
    let (body, content_type) =
        response_body::<H, Infallible>(accept, health).unwrap_or_else(|err| {
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
pub(crate) fn health_check_handler<State, H, F>(
    handler: impl 'static + Send + Sync + Fn(&State) -> F,
) -> HealthCheckHandler<State>
where
    State: 'static + Send + Sync,
    H: HealthCheck,
    F: 'static + Send + Future<Output = H>,
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
            health_check_response(&accept, health)
        }
        .boxed()
    })
}

pub(crate) fn response_body<T: Serialize, E>(
    accept: &Accept,
    body: T,
) -> Result<(Body, Mime), RouteError<E>> {
    let ty = best_response_type(accept, &[mime::JSON, mime::BYTE_STREAM])?;
    if ty == mime::BYTE_STREAM {
        let bytes = bincode::serialize(&body).map_err(RouteError::Bincode)?;
        Ok((bytes.into(), mime::BYTE_STREAM))
    } else if ty == mime::JSON {
        let json = serde_json::to_string(&body).map_err(RouteError::Json)?;
        Ok((json.into(), mime::JSON))
    } else {
        unreachable!()
    }
}

pub(crate) fn respond_with<T: Serialize, E>(
    accept: &Accept,
    body: T,
) -> Result<tide::Response, RouteError<E>> {
    let (body, content_type) = response_body(accept, body)?;
    Ok(tide::Response::builder(StatusCode::Ok)
        .body(body)
        .content_type(content_type)
        .build())
}
