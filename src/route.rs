use crate::{
    healthcheck::HealthCheck,
    request::{RequestParam, RequestParamType, RequestParams},
};
use async_trait::async_trait;
use futures::Future;
use serde::Serialize;
use snafu::Snafu;
use snafu::{OptionExt, Snafu};
use std::collections::HashMap;
use std::convert::Infallible;
use std::fmt::{self, Display, Formatter};
use std::marker::PhantomData;
use std::pin::Pin;
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

/// An error returned by a route handler.
///
/// [RouteError] encapsulates application specific errors `E` returned by the user-installed handler
/// itself. It also includes errors in the route dispatching logic, such as failures to turn the
/// result of the user-installed handler into an HTTP response.
pub enum RouteError<E> {
    AppSpecific(E),
    UnsupportedContentType,
    Bincode(bincode::Error),
    Json(serde_json::Error),
    Tide(tide::Error),
}

impl<E: Display> Display for RouteError<E> {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        match self {
            Self::AppSpecific(err) => write!(f, "{}", err),
            Self::UnsupportedContentType => write!(f, "requested content type is not supported"),
            Self::Bincode(err) => write!(f, "error creating byte stream: {}", err),
            Self::Json(err) => write!(f, "error creating JSON response: {}", err),
            Self::Tide(err) => write!(f, "{}", err),
        }
    }
}

impl<E> RouteError<E> {
    pub fn status(&self) -> StatusCode {
        match self {
            Self::UnsupportedContentType => StatusCode::BadRequest,
            _ => StatusCode::InternalServerError,
        }
    }

    pub fn map_app_specific<E2>(self, f: impl Fn(E) -> E2) -> RouteError<E2> {
        match self {
            RouteError::AppSpecific(e) => RouteError::AppSpecific(f(e)),
            RouteError::UnsupportedContentType => RouteError::UnsupportedContentType,
            RouteError::Bincode(err) => RouteError::Bincode(err),
            RouteError::Json(err) => RouteError::Json(err),
            RouteError::Tide(err) => RouteError::Tide(err),
        }
    }
}

/// A route handler.
///
/// The [Handler] trait defines the interface required of route handlers -- they must be able to
/// take [RequestParams] and produce either a response or an appropriately typed error.
///
/// Implementations of this trait are provided for handler functions returning a serializable
/// response ([FnHandler]),for boxed handlers, and for the [MapErr] type which can be used to
/// transform the `Error` type of a [Handler]. This trait is usually used as
/// `Box<dyn Handler<State, Error>>`, in order to type-erase route-specific details such as the
/// return type of a handler function. The types which are preserved, `State` and `Error`, should be
/// the same for all handlers in an API module.
#[async_trait]
pub(crate) trait Handler<State, Error>: 'static + Send + Sync {
    async fn handle(&self, req: RequestParams<State>) -> Result<tide::Response, RouteError<Error>>;
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
pub(crate) struct FnHandler<F, Fut, R> {
    f: F,
    _phantom: PhantomData<(Fut, R)>,
}

impl<F, Fut, R> From<F> for FnHandler<F, Fut, R> {
    fn from(f: F) -> Self {
        Self {
            f,
            _phantom: Default::default(),
        }
    }
}

#[async_trait]
impl<F, Fut, R, State, Error> Handler<State, Error> for FnHandler<F, Fut, R>
where
    F: 'static + Send + Sync + Fn(RequestParams<State>) -> Fut,
    Fut: 'static + Send + Sync + Future<Output = Result<R, Error>>,
    R: 'static + Send + Sync + Serialize,
    State: 'static + Send + Sync,
{
    async fn handle(&self, req: RequestParams<State>) -> Result<tide::Response, RouteError<Error>> {
        let mut accept = Accept::from_headers(req.headers()).map_err(RouteError::Tide)?;
        (self.f)(req)
            .await
            .map_err(RouteError::AppSpecific)
            .and_then(|res| respond_with(&mut accept, &res))
    }
}

#[async_trait]
impl<H: ?Sized + Handler<State, Error>, State: 'static + Send + Sync, Error> Handler<State, Error>
    for Box<H>
{
    async fn handle(&self, req: RequestParams<State>) -> Result<tide::Response, RouteError<Error>> {
        (**self).handle(req).await
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
    method: http::Method,
    doc: String,
    handler: Option<Box<dyn Handler<State, Error>>>,
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
    RouteMustBeTable,
}

impl<State, Error> Route<State, Error> {
    /// Parse a [Route] from a TOML specification.
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
        let mut pmap = HashMap::<String, RequestParam>::new();
        for path in paths.iter() {
            for seg in path.split('/') {
                if seg.starts_with(':') {
                    let ptype = RequestParamType::from_str(
                        spec[seg]
                            .as_str()
                            .ok_or(RouteParseError::InvalidTypeExpression)?,
                    )
                    .map_err(|_| RouteParseError::UnrecognizedType)?;
                    // TODO Should the map key and name be different? If
                    // not, then RequestParam::name is redundant.
                    pmap.insert(
                        seg.to_string(),
                        RequestParam {
                            name: seg.to_string(),
                            param_type: ptype,
                            // TODO How should we encode optioanl params?
                            required: true,
                        },
                    );
                }
            }
        }
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
            params: spec
                .as_table()
                .context(RouteMustBeTableSnafu)?
                .iter()
                .filter_map(|(key, val)| {
                    if !key.starts_with(':') {
                        return None;
                    }
                    let ty = match val.as_str() {
                        Some(ty) => match ty.parse() {
                            Ok(ty) => ty,
                            Err(_) => return Some(Err(RouteParseError::IncorrectParamType)),
                        },
                        None => return Some(Err(RouteParseError::IncorrectParamType)),
                    };
                    Some(Ok(RequestParam {
                        name: key[1..].to_string(),
                        param_type: ty,
                        required: true,
                    }))
                })
                .collect::<Result<_, _>>()?,
            method: match spec.get("METHOD") {
                Some(val) => val
                    .as_str()
                    .context(MethodMustBeStringSnafu)?
                    .parse()
                    .map_err(|_| RouteParseError::InvalidMethod)?,
                None => Method::Get,
            },
            doc: String::new(),
            handler: None,
        })
    }

    /// The name of the route.
    ///
    /// This is the name used to identify the route when binding a handler. It is also the first
    /// segment of all of the URL patterns for this route.
    pub fn name(&self) -> String {
        self.name.clone()
    }

    /// The HTTP method of the route.
    pub fn method(&self) -> http::Method {
        self.method
    }

    /// Whether a non-default handler has been bound to this route.
    pub fn has_handler(&self) -> bool {
        self.handler.is_some()
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
            handler: self.handler.map(|h| {
                let h: Box<dyn Handler<State, Error2>> =
                    Box::new(MapErr::<Box<dyn Handler<State, Error>>, _, Error>::new(
                        h, f,
                    ));
                h
            }),
            name: self.name,
            patterns: self.patterns,
            params: self.params,
            method: self.method,
            doc: self.doc,
        }
    }
}

impl<State, Error> Route<State, Error> {
    pub(crate) fn set_handler(&mut self, handler: impl Handler<State, Error>) {
        self.handler = Some(Box::new(handler));
    }

    pub(crate) fn set_fn_handler<F, Fut, T>(&mut self, handler: F)
    where
        F: 'static + Send + Sync + Fn(RequestParams<State>) -> Fut,
        Fut: 'static + Send + Sync + Future<Output = Result<T, Error>>,
        T: 'static + Send + Sync + Serialize,
        State: 'static + Send + Sync,
    {
        self.set_handler(FnHandler::from(handler))
    }

    pub(crate) fn default_handler(
        &self,
        _req: RequestParams<State>,
    ) -> Result<tide::Response, RouteError<Error>> {
        unimplemented!()
    }
}

#[async_trait]
impl<State, Error> Handler<State, Error> for Route<State, Error>
where
    Error: 'static,
    State: 'static + Send + Sync,
{
    async fn handle(&self, req: RequestParams<State>) -> Result<tide::Response, RouteError<Error>> {
        match &self.handler {
            Some(handler) => handler.handle(req).await,
            None => self.default_handler(req),
        }
    }
}

pub struct MapErr<H, F, E> {
    handler: H,
    map: F,
    _phantom: std::marker::PhantomData<E>,
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
        req: RequestParams<State>,
    ) -> Result<tide::Response, RouteError<Error2>> {
        self.handler
            .handle(req)
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
pub(crate) type HealthCheckHandler<State> = Box<
    dyn 'static
        + Send
        + Sync
        + Fn(RequestParams<State>) -> Pin<Box<dyn Send + Future<Output = tide::Response>>>,
>;

pub(crate) fn health_check_response<H: HealthCheck>(
    accept: &mut Option<Accept>,
    health: H,
) -> tide::Response {
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
    Box::new(move |req| {
        let mut accept = Accept::from_headers(req.headers()).ok().flatten();
        let future = handler(req.state());
        Box::pin(async move {
            let health = future.await;
            health_check_response(&mut accept, health)
        })
    })
}

fn best_response_type<E>(
    accept: &mut Option<Accept>,
    available: &[Mime],
) -> Result<Mime, RouteError<E>> {
    match accept {
        Some(accept) => {
            // The Accept type has a `negotiate` method, but it doesn't properly handle
            // wildcards. It handles * but not */* and basetype/*, because for content type
            // proposals like */* and basetype/*, it looks for a literal match in `available`,
            // it does not perform pattern matching. So, we implement negotiation ourselves.
            //
            // First sort by the weight parameter, which the Accept type does do correctly.
            accept.sort();
            // Go through each proposed content type, in the order specified by the client, and
            // match them against our available types, respecting wildcards.
            for proposed in accept.iter() {
                if proposed.basetype() == "*" {
                    // The only acceptable Accept value with a basetype of * is */*, therefore
                    // this will match any available type.
                    return Ok(available[0].clone());
                } else if proposed.subtype() == "*" {
                    // If the subtype is * but the basetype is not, look for a proposed type
                    // with a matching basetype and any subtype.
                    for mime in available {
                        if mime.basetype() == proposed.basetype() {
                            return Ok(mime.clone());
                        }
                    }
                } else if available.contains(proposed) {
                    // If neither part of the proposal is a wildcard, look for a literal match.
                    return Ok((**proposed).clone());
                }
            }

            if accept.wildcard() {
                // If no proposals are available but a wildcard flag * was given, return any
                // available content type.
                Ok(available[0].clone())
            } else {
                Err(RouteError::UnsupportedContentType)
            }
        }
        None => {
            // If no content type is explicitly requested, default to the first available type.
            Ok(available[0].clone())
        }
    }
}

fn response_body<T: Serialize, E>(
    accept: &mut Option<Accept>,
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

fn respond_with<T: Serialize, E>(
    accept: &mut Option<Accept>,
    body: T,
) -> Result<tide::Response, RouteError<E>> {
    let (body, content_type) = response_body(accept, body)?;
    Ok(tide::Response::builder(StatusCode::Ok)
        .body(body)
        .content_type(content_type)
        .build())
}
