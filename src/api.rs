// COPYRIGHT100 (c) 2022 Espresso Systems (espressosys.com)
//
// This program is free software: you can redistribute it and/or modify it under the terms of the
// GNU General Public License as published by the Free Software Foundation, either version 3 of the
// License, or (at your option) any later version.
//
// This program is distributed in the hope that it will be useful, but WITHOUT ANY WARRANTY; without
// even the implied warranty of MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE. See the GNU
// General Public License for more details.
//
// You should have received a copy of the GNU General Public License along with this program. If
// not, see <https://www.gnu.org/licenses/>.

use crate::{
    healthcheck::{HealthCheck, HealthStatus},
    method::{method_is_mutable, ReadState, WriteState},
    request::RequestParams,
    route::{self, *},
};
use async_trait::async_trait;
use derive_more::From;
use futures::future::{BoxFuture, Future};
use semver::Version;
use serde::{Deserialize, Serialize};
use serde_with::{serde_as, DisplayFromStr};
use snafu::{OptionExt, ResultExt, Snafu};
use std::collections::hash_map::{HashMap, IntoValues, Values};
use std::ops::Index;
use tide::http::{content::Accept, Method};

/// An error encountered when parsing or constructing an [Api].
#[derive(Clone, Debug, Snafu)]
pub enum ApiError {
    Route { source: RouteParseError },
    MissingRoutesTable,
    RoutesMustBeTable,
    UndefinedRoute,
    HandlerAlreadyRegistered,
    IncorrectMethod { expected: Method, actual: Method },
    MetaMustBeTable,
    MissingMetaTable,
    MissingFormatVersion,
    InvalidFormatVersion,
}

/// Version information about an API.
#[serde_as]
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct ApiVersion {
    /// The version of this API.
    #[serde_as(as = "Option<DisplayFromStr>")]
    pub api_version: Option<Version>,

    /// The format version of the TOML specification used to load this API.
    #[serde_as(as = "DisplayFromStr")]
    pub spec_version: Version,
}

/// A description of an API.
///
/// An [Api] is a structured representation of an `api.toml` specification. It contains API-level
/// metadata and descriptions of all of the routes in the specification. It can be parsed from a
/// TOML file and registered as a module of an [App](crate::App).
pub struct Api<State, Error> {
    routes: HashMap<String, Route<State, Error>>,
    health_check: Option<HealthCheckHandler<State>>,
    version: ApiVersion,
}

impl<'a, State, Error> IntoIterator for &'a Api<State, Error> {
    type Item = &'a Route<State, Error>;
    type IntoIter = Values<'a, String, Route<State, Error>>;

    fn into_iter(self) -> Self::IntoIter {
        self.routes.values()
    }
}

impl<State, Error> IntoIterator for Api<State, Error> {
    type Item = Route<State, Error>;
    type IntoIter = IntoValues<String, Route<State, Error>>;

    fn into_iter(self) -> Self::IntoIter {
        self.routes.into_values()
    }
}

impl<State, Error> Index<&str> for Api<State, Error> {
    type Output = Route<State, Error>;

    fn index(&self, index: &str) -> &Route<State, Error> {
        &self.routes[index]
    }
}

impl<State, Error> Api<State, Error> {
    /// Parse an API from a TOML specification.
    pub fn new(api: toml::Value) -> Result<Self, ApiError> {
        let routes = match api.get("route") {
            Some(routes) => routes.as_table().context(RoutesMustBeTableSnafu)?,
            None => return Err(ApiError::MissingRoutesTable),
        };
        let meta = match api.get("meta") {
            Some(meta) => meta.as_table().context(MetaMustBeTableSnafu)?,
            None => return Err(ApiError::MissingMetaTable),
        };
        Ok(Self {
            routes: routes
                .into_iter()
                .map(|(name, spec)| {
                    let route = Route::new(name.clone(), spec).context(RouteSnafu)?;
                    Ok((route.name(), route))
                })
                .collect::<Result<_, _>>()?,
            health_check: None,
            version: ApiVersion {
                api_version: None,
                spec_version: meta
                    .get("FORMAT_VERSION")
                    .context(MissingFormatVersionSnafu)?
                    .as_str()
                    .context(InvalidFormatVersionSnafu)?
                    .parse()
                    .map_err(|_| ApiError::InvalidFormatVersion)?,
            },
        })
    }

    /// Set the API version.
    ///
    /// The version information will automatically be included in responses to `GET /version`.
    ///
    /// This is the version of the application or sub-application which this instance of [Api]
    /// represents. The versioning encompasses both the API specification passed to [new](Api::new)
    /// and the Rust crate implementing the route handlers for the API. Changes to either of
    /// these components should result in a change to the version.
    ///
    /// Since the API specification and the route handlers are usually packaged together, and since
    /// Rust crates are versioned anyways using Cargo, it is a good idea to use the version of the
    /// API crate found in Cargo.toml. This can be automatically found at build time using the
    /// environment variable `CARGO_PKG_VERSION` and the [env] macro. As long as the following code
    /// is contained in the API crate, it should result in a reasonable version:
    ///
    /// ```
    /// # fn ex(api: &mut tide_disco::Api<(), ()>) {
    /// api.with_version(env!("CARGO_PKG_VERSION").parse().unwrap());
    /// # }
    /// ```
    pub fn with_version(&mut self, version: Version) -> &mut Self {
        self.version.api_version = Some(version);
        self
    }

    /// Register a handler for a route.
    ///
    /// When the server receives a request whose URL matches the pattern of the route `name`,
    /// `handler` will be invoked with the parameters of the request and a reference to the current
    /// state, and the result will be serialized into a response.
    ///
    /// # Examples
    ///
    /// A simple getter route for a state object.
    ///
    /// `api.toml`
    ///
    /// ```toml
    /// [route.getstate]
    /// PATH = ["/getstate"]
    /// DOC = "Gets the current state."
    /// ```
    ///
    /// ```
    /// use futures::FutureExt;
    /// # use tide_disco::Api;
    ///
    /// type State = u64;
    ///
    /// # fn ex(api: &mut Api<State, ()>) {
    /// api.at("getstate", |req, state| async { Ok(*state) }.boxed());
    /// # }
    /// ```
    ///
    /// A counter endpoint which increments a mutable state. Notice how we use `METHOD = "POST"` to
    /// ensure that the HTTP method for this route is compatible with mutable access.
    ///
    /// `api.toml`
    ///
    /// ```toml
    /// [route.increment]
    /// PATH = ["/increment"]
    /// METHOD = "POST"
    /// DOC = "Increment the current state and return the new value."
    /// ```
    ///
    /// ```
    /// use async_std::sync::Mutex;
    /// use futures::FutureExt;
    /// # use tide_disco::Api;
    ///
    /// type State = Mutex<u64>;
    ///
    /// # fn ex(api: &mut Api<State, ()>) {
    /// api.at("increment", |req, state| async {
    ///     let mut guard = state.lock().await;
    ///     *guard += 1;
    ///     Ok(*guard)
    /// }.boxed());
    /// # }
    /// ```
    ///
    /// # Warnings
    /// The route will use the HTTP method specified in the TOML specification for the named route
    /// (or GET if the method is not specified). Some HTTP methods imply constraints on mutability.
    /// For example, GET routes must be "pure", and not mutate any server state. Violating this
    /// constraint may lead to confusing and unpredictable behavior. If the `State` type has
    /// interior mutability (for instance, [RwLock](async_std::sync::RwLock)) it is up to the
    /// `handler` not to use the interior mutability if the HTTP method suggests it shouldn't.
    ///
    /// If you know the HTTP method when you are registering the route, we recommend you use the
    /// safer versions of this function, which enforce the appropriate mutability constraints. For
    /// example,
    /// * [get](Self::get)
    /// * [post](Self::post)
    /// * [put](Self::put)
    /// * [delete](Self::delete)
    ///
    /// # Errors
    ///
    /// If the route `name` does not exist in the API specification, or if the route already has a
    /// handler registered, an error is returned. Note that all routes are initialized with a
    /// default handler that echoes parameters and shows documentation, but this default handler can
    /// replaced by this function without raising [ApiError::HandlerAlreadyRegistered].
    ///
    /// # Limitations
    ///
    /// [Like many function parameters](crate#boxed-futures) in [tide_disco](crate), the
    /// handler function is required to return a [BoxFuture].
    pub fn at<F, T>(&mut self, name: &str, handler: F) -> Result<&mut Self, ApiError>
    where
        F: 'static + Send + Sync + Fn(RequestParams, &State) -> BoxFuture<'_, Result<T, Error>>,
        T: Serialize,
        State: 'static + Send + Sync,
    {
        let route = self.routes.get_mut(name).ok_or(ApiError::UndefinedRoute)?;
        if route.has_handler() {
            return Err(ApiError::HandlerAlreadyRegistered);
        } else {
            route.set_fn_handler(handler);
        }
        Ok(self)
    }

    fn method_immutable<F, T>(
        &mut self,
        method: Method,
        name: &str,
        handler: F,
    ) -> Result<&mut Self, ApiError>
    where
        F: 'static
            + Send
            + Sync
            + Fn(RequestParams, &<State as ReadState>::State) -> BoxFuture<'_, Result<T, Error>>,
        T: Serialize,
        State: 'static + Send + Sync + ReadState,
    {
        assert!(!method_is_mutable(method));
        let route = self.routes.get_mut(name).ok_or(ApiError::UndefinedRoute)?;
        if route.method() != method {
            return Err(ApiError::IncorrectMethod {
                expected: method,
                actual: route.method(),
            });
        }
        if route.has_handler() {
            return Err(ApiError::HandlerAlreadyRegistered);
        }
        route.set_handler(ReadHandler::from(handler));
        Ok(self)
    }

    /// Register a handler for a GET route.
    ///
    /// When the server receives a GET request whose URL matches the pattern of the route `name`,
    /// `handler` will be invoked with the parameters of the request and immutable access to the
    /// current state, and the result will be serialized into a response.
    ///
    /// The [ReadState] trait is used to acquire immutable access to the state, so the state
    /// reference passed to `handler` is actually [`<State as ReadState>::State`](ReadState::State).
    /// For example, if `State` is `RwLock<T>`, the lock will automatically be acquired for reading,
    /// and the handler will be passed a `&T`.
    ///
    /// # Examples
    ///
    /// A simple getter route for a state object.
    ///
    /// `api.toml`
    ///
    /// ```toml
    /// [route.getstate]
    /// PATH = ["/getstate"]
    /// DOC = "Gets the current state."
    /// ```
    ///
    /// ```
    /// use async_std::sync::RwLock;
    /// use futures::FutureExt;
    /// # use tide_disco::Api;
    ///
    /// type State = RwLock<u64>;
    ///
    /// # fn ex(api: &mut Api<State, ()>) {
    /// api.get("getstate", |req, state| async { Ok(*state) }.boxed());
    /// # }
    /// ```
    ///
    /// # Errors
    ///
    /// If the route `name` does not exist in the API specification, or if the route already has a
    /// handler registered, an error is returned. Note that all routes are initialized with a
    /// default handler that echoes parameters and shows documentation, but this default handler can
    /// replaced by this function without raising [ApiError::HandlerAlreadyRegistered].
    ///
    /// If the route `name` exists, but the method is not GET (that is, `METHOD = "M"` was used in
    /// the route definition in `api.toml`, with `M` other than `GET`) the error
    /// [IncorrectMethod](ApiError::IncorrectMethod) is returned.
    ///
    /// # Limitations
    ///
    /// [Like many function parameters](crate#boxed-futures) in [tide_disco](crate), the
    /// handler function is required to return a [BoxFuture].
    pub fn get<F, T>(&mut self, name: &str, handler: F) -> Result<&mut Self, ApiError>
    where
        F: 'static
            + Send
            + Sync
            + Fn(RequestParams, &<State as ReadState>::State) -> BoxFuture<'_, Result<T, Error>>,
        T: Serialize,
        State: 'static + Send + Sync + ReadState,
    {
        self.method_immutable(Method::Get, name, handler)
    }

    fn method_mutable<F, T>(
        &mut self,
        method: Method,
        name: &str,
        handler: F,
    ) -> Result<&mut Self, ApiError>
    where
        F: 'static
            + Send
            + Sync
            + Fn(RequestParams, &mut <State as ReadState>::State) -> BoxFuture<'_, Result<T, Error>>,
        T: Serialize,
        State: 'static + Send + Sync + WriteState,
    {
        assert!(method_is_mutable(method));
        let route = self.routes.get_mut(name).ok_or(ApiError::UndefinedRoute)?;
        if route.method() != method {
            return Err(ApiError::IncorrectMethod {
                expected: method,
                actual: route.method(),
            });
        }
        if route.has_handler() {
            return Err(ApiError::HandlerAlreadyRegistered);
        }
        route.set_handler(WriteHandler::from(handler));
        Ok(self)
    }

    /// Register a handler for a POST route.
    ///
    /// When the server receives a POST request whose URL matches the pattern of the route `name`,
    /// `handler` will be invoked with the parameters of the request and exclusive, mutable access
    /// to the current state, and the result will be serialized into a response.
    ///
    /// The [WriteState] trait is used to acquire mutable access to the state, so the state
    /// reference passed to `handler` is actually [`<State as ReadState>::State`](ReadState::State).
    /// For example, if `State` is `RwLock<T>`, the lock will automatically be acquired for writing,
    /// and the handler will be passed a `&mut T`.
    ///
    /// # Examples
    ///
    /// A counter endpoint which increments the state and returns the new state.
    ///
    /// `api.toml`
    ///
    /// ```toml
    /// [route.increment]
    /// PATH = ["/increment"]
    /// METHOD = "POST"
    /// DOC = "Increment the current state and return the new value."
    /// ```
    ///
    /// ```
    /// use async_std::sync::RwLock;
    /// use futures::FutureExt;
    /// # use tide_disco::Api;
    ///
    /// type State = RwLock<u64>;
    ///
    /// # fn ex(api: &mut Api<State, ()>) {
    /// api.post("increment", |req, state| async {
    ///     *state += 1;
    ///     Ok(*state)
    /// }.boxed());
    /// # }
    /// ```
    ///
    /// # Errors
    ///
    /// If the route `name` does not exist in the API specification, or if the route already has a
    /// handler registered, an error is returned. Note that all routes are initialized with a
    /// default handler that echoes parameters and shows documentation, but this default handler can
    /// replaced by this function without raising [ApiError::HandlerAlreadyRegistered].
    ///
    /// If the route `name` exists, but the method is not POST (that is, `METHOD = "M"` was used in
    /// the route definition in `api.toml`, with `M` other than `POST`) the error
    /// [IncorrectMethod](ApiError::IncorrectMethod) is returned.
    ///
    /// # Limitations
    ///
    /// [Like many function parameters](crate#boxed-futures) in [tide_disco](crate), the
    /// handler function is required to return a [BoxFuture].
    pub fn post<F, T>(&mut self, name: &str, handler: F) -> Result<&mut Self, ApiError>
    where
        F: 'static
            + Send
            + Sync
            + Fn(RequestParams, &mut <State as ReadState>::State) -> BoxFuture<'_, Result<T, Error>>,
        T: Serialize,
        State: 'static + Send + Sync + WriteState,
    {
        self.method_mutable(Method::Post, name, handler)
    }

    /// Register a handler for a PUT route.
    ///
    /// When the server receives a PUT request whose URL matches the pattern of the route `name`,
    /// `handler` will be invoked with the parameters of the request and exclusive, mutable access
    /// to the current state, and the result will be serialized into a response.
    ///
    /// The [WriteState] trait is used to acquire mutable access to the state, so the state
    /// reference passed to `handler` is actually [`<State as ReadState>::State`](ReadState::State).
    /// For example, if `State` is `RwLock<T>`, the lock will automatically be acquired for writing,
    /// and the handler will be passed a `&mut T`.
    ///
    /// # Examples
    ///
    /// An endpoint which replaces the current state with a new value.
    ///
    /// `api.toml`
    ///
    /// ```toml
    /// [route.replace]
    /// PATH = ["/replace/:new_state"]
    /// METHOD = "PUT"
    /// ":new_state" = "Integer"
    /// DOC = "Set the state to `:new_state`."
    /// ```
    ///
    /// ```
    /// use async_std::sync::RwLock;
    /// use futures::FutureExt;
    /// # use tide_disco::Api;
    ///
    /// type State = RwLock<u64>;
    ///
    /// # fn ex(api: &mut Api<State, tide_disco::RequestError>) {
    /// api.post("replace", |req, state| async move {
    ///     *state = req.u64_param("new_state")?;
    ///     Ok(())
    /// }.boxed());
    /// # }
    /// ```
    ///
    /// # Errors
    ///
    /// If the route `name` does not exist in the API specification, or if the route already has a
    /// handler registered, an error is returned. Note that all routes are initialized with a
    /// default handler that echoes parameters and shows documentation, but this default handler can
    /// replaced by this function without raising [ApiError::HandlerAlreadyRegistered].
    ///
    /// If the route `name` exists, but the method is not PUT (that is, `METHOD = "M"` was used in
    /// the route definition in `api.toml`, with `M` other than `PUT`) the error
    /// [IncorrectMethod](ApiError::IncorrectMethod) is returned.
    ///
    /// # Limitations
    ///
    /// [Like many function parameters](crate#boxed-futures) in [tide_disco](crate), the
    /// handler function is required to return a [BoxFuture].
    pub fn put<F, T>(&mut self, name: &str, handler: F) -> Result<&mut Self, ApiError>
    where
        F: 'static
            + Send
            + Sync
            + Fn(RequestParams, &mut <State as ReadState>::State) -> BoxFuture<'_, Result<T, Error>>,
        T: Serialize,
        State: 'static + Send + Sync + WriteState,
    {
        self.method_mutable(Method::Put, name, handler)
    }

    /// Register a handler for a DELETE route.
    ///
    /// When the server receives a DELETE request whose URL matches the pattern of the route `name`,
    /// `handler` will be invoked with the parameters of the request and exclusive, mutable access
    /// to the current state, and the result will be serialized into a response.
    ///
    /// The [WriteState] trait is used to acquire mutable access to the state, so the state
    /// reference passed to `handler` is actually [`<State as ReadState>::State`](ReadState::State).
    /// For example, if `State` is `RwLock<T>`, the lock will automatically be acquired for writing,
    /// and the handler will be passed a `&mut T`.
    ///
    /// # Examples
    ///
    /// An endpoint which clears the current state.
    ///
    /// `api.toml`
    ///
    /// ```toml
    /// [route.state]
    /// PATH = ["/state"]
    /// METHOD = "DELETE"
    /// DOC = "Clear the state."
    /// ```
    ///
    /// ```
    /// use async_std::sync::RwLock;
    /// use futures::FutureExt;
    /// # use tide_disco::Api;
    ///
    /// type State = RwLock<Option<u64>>;
    ///
    /// # fn ex(api: &mut Api<State, ()>) {
    /// api.delete("state", |req, state| async {
    ///     *state = None;
    ///     Ok(())
    /// }.boxed());
    /// # }
    /// ```
    ///
    /// # Errors
    ///
    /// If the route `name` does not exist in the API specification, or if the route already has a
    /// handler registered, an error is returned. Note that all routes are initialized with a
    /// default handler that echoes parameters and shows documentation, but this default handler can
    /// replaced by this function without raising [ApiError::HandlerAlreadyRegistered].
    ///
    /// If the route `name` exists, but the method is not DELETE (that is, `METHOD = "M"` was used
    /// in the route definition in `api.toml`, with `M` other than `DELETE`) the error
    /// [IncorrectMethod](ApiError::IncorrectMethod) is returned.
    ///
    /// # Limitations
    ///
    /// [Like many function parameters](crate#boxed-futures) in [tide_disco](crate), the
    /// handler function is required to return a [BoxFuture].
    pub fn delete<F, T>(&mut self, name: &str, handler: F) -> Result<&mut Self, ApiError>
    where
        F: 'static
            + Send
            + Sync
            + Fn(RequestParams, &mut <State as ReadState>::State) -> BoxFuture<'_, Result<T, Error>>,
        T: Serialize,
        State: 'static + Send + Sync + WriteState,
    {
        self.method_mutable(Method::Delete, name, handler)
    }

    /// Set the health check handler for this API.
    ///
    /// This overrides the existing handler. If `health_check` has not yet been called, the default
    /// handler is one which simply returns `Health::default()`.
    pub async fn with_health_check<H, F>(
        &mut self,
        handler: impl 'static + Send + Sync + Fn(&State) -> F,
    ) -> &mut Self
    where
        State: 'static + Send + Sync,
        H: HealthCheck,
        F: 'static + Send + Future<Output = H>,
    {
        self.health_check = Some(route::health_check_handler(handler));
        self
    }

    /// Check the health status of a server with the given state.
    pub async fn health(&self, req: RequestParams, state: &State) -> tide::Response {
        if let Some(handler) = &self.health_check {
            handler(req, state).await
        } else {
            // If there is no healthcheck handler registered, just return [HealthStatus::Available]
            // by default; after all, if this handler is getting hit at all, the service must be up.
            route::health_check_response(
                &req.accept().unwrap_or_else(|_| {
                    // The healthcheck endpoint is not allowed to fail, so just use the default content
                    // type if we can't parse the Accept header.
                    let mut accept = Accept::new();
                    accept.set_wildcard(true);
                    accept
                }),
                HealthStatus::Available,
            )
        }
    }

    /// Get the version of this API.
    pub fn version(&self) -> ApiVersion {
        self.version.clone()
    }

    /// Create a new [Api] which is just like this one, except has a transformed `Error` type.
    pub fn map_err<Error2>(
        self,
        f: impl 'static + Clone + Send + Sync + Fn(Error) -> Error2,
    ) -> Api<State, Error2>
    where
        Error: 'static + Send + Sync,
        Error2: 'static,
        State: 'static + Send + Sync,
    {
        Api {
            routes: self
                .routes
                .into_iter()
                .map(|(name, route)| (name, route.map_err(f.clone())))
                .collect(),
            health_check: self.health_check,
            version: self.version,
        }
    }
}

// `ReadHandler { handler }` essentially represents a handler function
// `move |req, state| async { state.read(|state| handler(req, state)).await.await }`. However, I
// cannot convince Rust that the future returned by this closure moves out of `req` while borrowing
// from `handler`, which is owned by the closure itself and thus outlives the closure body. This is
// partly due to the limitation where _all_ closure parameters must be captured either by value or
// by reference, and probably partly due to my lack of creativity. In any case, writing out the
// closure object and [Handler] implementation by hand seems to convince Rust that this code is
// memory safe.
#[derive(From)]
struct ReadHandler<F> {
    handler: F,
}

#[async_trait]
impl<State, Error, F, R> Handler<State, Error> for ReadHandler<F>
where
    F: 'static
        + Send
        + Sync
        + Fn(RequestParams, &<State as ReadState>::State) -> BoxFuture<'_, Result<R, Error>>,
    R: Serialize,
    State: 'static + Send + Sync + ReadState,
{
    async fn handle(
        &self,
        req: RequestParams,
        state: &State,
    ) -> Result<tide::Response, RouteError<Error>> {
        let accept = req.accept()?;
        response_from_result(
            &accept,
            state.read(|state| (self.handler)(req, state)).await,
        )
    }
}

// A manual closure that serves a similar purpose as [ReadHandler].
#[derive(From)]
struct WriteHandler<F> {
    handler: F,
}

#[async_trait]
impl<State, Error, F, R> Handler<State, Error> for WriteHandler<F>
where
    F: 'static
        + Send
        + Sync
        + Fn(RequestParams, &mut <State as ReadState>::State) -> BoxFuture<'_, Result<R, Error>>,
    R: Serialize,
    State: 'static + Send + Sync + WriteState,
{
    async fn handle(
        &self,
        req: RequestParams,
        state: &State,
    ) -> Result<tide::Response, RouteError<Error>> {
        let accept = req.accept()?;
        response_from_result(
            &accept,
            state.write(|state| (self.handler)(req, state)).await,
        )
    }
}
