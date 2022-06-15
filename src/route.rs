use crate::request::{RequestParam, RequestParams};
use async_trait::async_trait;
use futures::Future;
use serde::Serialize;
use snafu::Snafu;
use std::fmt::{self, Display, Formatter};
use std::marker::PhantomData;
use tide::http::{
    self,
    content::Accept,
    mime::{self, Mime},
    StatusCode,
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
pub enum RouteParseError {}

impl<State, Error> Route<State, Error> {
    /// Parse a [Route] from a TOML specification.
    pub fn new(name: String, spec: &toml::Value) -> Result<Self, RouteParseError> {
        unimplemented!("route parsing")
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

fn respond_with<T: Serialize, E>(
    accept: &mut Option<Accept>,
    body: T,
) -> Result<tide::Response, RouteError<E>> {
    let ty = best_response_type(accept, &[mime::JSON, mime::BYTE_STREAM])?;
    if ty == mime::BYTE_STREAM {
        let bytes = bincode::serialize(&body).map_err(RouteError::Bincode)?;
        Ok(tide::Response::builder(tide::StatusCode::Ok)
            .body(bytes)
            .content_type(mime::BYTE_STREAM)
            .build())
    } else if ty == mime::JSON {
        Ok(tide::Response::builder(tide::StatusCode::Ok)
            .body(serde_json::to_string(&body).map_err(RouteError::Json)?)
            .content_type(mime::JSON)
            .build())
    } else {
        unreachable!()
    }
}
