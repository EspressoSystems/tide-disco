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
    method::{Method, ReadState, WriteState},
    request::RequestParams,
    route::{self, *},
    socket,
};
use async_trait::async_trait;
use derive_more::From;
use futures::{
    future::{BoxFuture, Future},
    stream::BoxStream,
};
use semver::Version;
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use serde_with::{serde_as, DisplayFromStr};
use snafu::{OptionExt, ResultExt, Snafu};
use std::collections::hash_map::{Entry, HashMap, IntoValues, Values};
use std::fmt::Display;
use std::ops::Index;
use tide::http::content::Accept;

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
    AmbiguousRoutes { route1: String, route2: String },
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
    routes_by_path: HashMap<String, Vec<String>>,
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

/// Iterator for [routes_by_path](Api::routes_by_path).
///
/// This type iterates over all of the routes that have a given path.
/// [routes_by_path](Api::routes_by_path), in turn, returns an iterator over paths whose items
/// contain a [RoutesWithPath] iterator.
pub struct RoutesWithPath<'a, State, Error> {
    routes: std::slice::Iter<'a, String>,
    api: &'a Api<State, Error>,
}

impl<'a, State, Error> Iterator for RoutesWithPath<'a, State, Error> {
    type Item = &'a Route<State, Error>;

    fn next(&mut self) -> Option<Self::Item> {
        Some(&self.api.routes[self.routes.next()?])
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
        // Collect routes into a [HashMap] indexed by route name.
        let routes = routes
            .into_iter()
            .map(|(name, spec)| {
                let route = Route::new(name.clone(), spec).context(RouteSnafu)?;
                Ok((route.name(), route))
            })
            .collect::<Result<HashMap<_, _>, _>>()?;
        // Collect routes into groups of route names indexed by route pattern.
        let mut routes_by_path = HashMap::new();
        for route in routes.values() {
            for path in route.patterns() {
                match routes_by_path.entry(path.clone()) {
                    Entry::Vacant(e) => e.insert(Vec::new()).push(route.name().clone()),
                    Entry::Occupied(mut e) => {
                        // If there is already a route with this path and method, then dispatch is
                        // ambiguous.
                        if let Some(ambiguous_name) = e
                            .get()
                            .iter()
                            .find(|name| routes[*name].method() == route.method())
                        {
                            return Err(ApiError::AmbiguousRoutes {
                                route1: route.name(),
                                route2: ambiguous_name.clone(),
                            });
                        }
                        e.get_mut().push(route.name());
                    }
                }
            }
        }
        Ok(Self {
            routes,
            routes_by_path,
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

    /// Iterate over groups of routes with the same path.
    pub fn routes_by_path(&self) -> impl Iterator<Item = (&str, RoutesWithPath<'_, State, Error>)> {
        self.routes_by_path.iter().map(|(path, routes)| {
            (
                path.as_str(),
                RoutesWithPath {
                    routes: routes.iter(),
                    api: self,
                },
            )
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
    /// If the route `name` exists, but it is not an HTTP route (for example, `METHOD = "SOCKET"`
    /// was used when defining the route in the API specification), [ApiError::IncorrectMethod] is
    /// returned.
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
        }

        if !route.method().is_http() {
            return Err(ApiError::IncorrectMethod {
                // Just pick any HTTP method as the expected method.
                expected: Method::get(),
                actual: route.method(),
            });
        }

        // `set_fn_handler` only fails if the route is not an HTTP route; since we have already
        // checked that it is, this cannot fail.
        route
            .set_fn_handler(handler)
            .unwrap_or_else(|_| panic!("unexpected failure in set_fn_handler"));

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
        assert!(method.is_http() && !method.is_mutable());
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
        // `set_handler` only fails if the route is not an HTTP route; since we have already checked
        // that it is, this cannot fail.
        route
            .set_handler(ReadHandler::from(handler))
            .unwrap_or_else(|_| panic!("unexpected failure in set_handler"));
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
        self.method_immutable(Method::get(), name, handler)
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
        assert!(method.is_http() && method.is_mutable());
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

        // `set_handler` only fails if the route is not an HTTP route; since we have already checked
        // that it is, this cannot fail.
        route
            .set_handler(WriteHandler::from(handler))
            .unwrap_or_else(|_| panic!("unexpected failure in set_handler"));
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
        self.method_mutable(Method::post(), name, handler)
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
        self.method_mutable(Method::put(), name, handler)
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
        self.method_mutable(Method::delete(), name, handler)
    }

    /// Register a handler for a SOCKET route.
    ///
    /// When the server receives any request whose URL matches the pattern for this route and which
    /// includes the WebSockets upgrade headers, the server will negotiate a protocol upgrade with
    /// the client, establishing a WebSockets connection, and then invoke `handler`. `handler` will
    /// be given the parameters of the request which initiated the connection and a reference to the
    /// application state, as well as a [Connection](socket::Connection) object which it can then
    /// use for asynchronous, bi-directional communication with the client.
    ///
    /// The server side of the connection will remain open as long as the future returned by
    /// `handler` is remains unresolved. The handler can terminate the connection by returning. If
    /// it returns an error, the error message will be included in the
    /// [CloseFrame](tide_websockets::tungstenite::protocol::CloseFrame) sent to the client when
    /// tearing down the connection.
    ///
    /// # Examples
    ///
    /// A socket endpoint which receives amounts from the client and returns a running sum.
    ///
    /// `api.toml`
    ///
    /// ```toml
    /// [route.sum]
    /// PATH = ["/sum"]
    /// METHOD = "SOCKET"
    /// DOC = "Stream a running sum."
    /// ```
    ///
    /// ```
    /// use futures::{FutureExt, SinkExt, StreamExt};
    /// use tide_disco::{error::ServerError, socket::Connection, Api};
    ///
    /// # fn ex(api: &mut Api<(), ServerError>) {
    /// api.socket("sum", |_req, mut conn: Connection<i32, i32, ServerError>, _state| async move {
    ///     let mut sum = 0;
    ///     while let Some(amount) = conn.next().await {
    ///         sum += amount?;
    ///         conn.send(&sum).await?;
    ///     }
    ///     Ok(())
    /// }.boxed());
    /// # }
    /// ```
    //
    /// # Errors
    ///
    /// If the route `name` does not exist in the API specification, or if the route already has a
    /// handler registered, an error is returned. Note that all routes are initialized with a
    /// default handler that echoes parameters and shows documentation, but this default handler can
    /// replaced by this function without raising [ApiError::HandlerAlreadyRegistered].
    ///
    /// If the route `name` exists, but the method is not SOCKET (that is, `METHOD = "M"` was used
    /// in the route definition in `api.toml`, with `M` other than `SOCKET`) the error
    /// [IncorrectMethod](ApiError::IncorrectMethod) is returned.
    ///
    /// # Limitations
    ///
    /// [Like many function parameters](crate#boxed-futures) in [tide_disco](crate), the
    /// handler function is required to return a [BoxFuture].
    pub fn socket<F, ToClient, FromClient>(
        &mut self,
        name: &str,
        handler: F,
    ) -> Result<&mut Self, ApiError>
    where
        F: 'static
            + Send
            + Sync
            + Fn(
                RequestParams,
                socket::Connection<ToClient, FromClient, Error>,
                &State,
            ) -> BoxFuture<'_, Result<(), Error>>,
        ToClient: 'static + Serialize + ?Sized,
        FromClient: 'static + DeserializeOwned,
        State: 'static + Send + Sync,
        Error: 'static + Send + Display,
    {
        self.register_socket_handler(name, socket::handler(handler))
    }

    /// Register a uni-directional handler for a SOCKET route.
    ///
    /// This function is very similar to [socket](Self::socket), but it permits the handler only to
    /// send messages to the client, not to receive messages back. As such, the handler does not
    /// take a [Connection](socket::Connection). Instead, it simply returns a stream of messages
    /// which are forwarded to the client as they are generated. If the stream ever yields an error,
    /// the error is propagated to the client and then the connection is closed.
    ///
    /// This function can be simpler to use than [socket](Self::socket) in case the handler does not
    /// need to receive messages from the client.
    pub fn stream<F, Msg>(&mut self, name: &str, handler: F) -> Result<&mut Self, ApiError>
    where
        F: 'static + Send + Sync + Fn(RequestParams, &State) -> BoxStream<Result<Msg, Error>>,
        Msg: 'static + Serialize + Send + Sync,
        State: 'static + Send + Sync,
        Error: 'static + Send + Display,
    {
        self.register_socket_handler(name, socket::stream_handler(handler))
    }

    fn register_socket_handler(
        &mut self,
        name: &str,
        handler: socket::Handler<State, Error>,
    ) -> Result<&mut Self, ApiError> {
        let route = self.routes.get_mut(name).ok_or(ApiError::UndefinedRoute)?;
        if route.method() != Method::Socket {
            return Err(ApiError::IncorrectMethod {
                expected: Method::Socket,
                actual: route.method(),
            });
        }
        if route.has_handler() {
            return Err(ApiError::HandlerAlreadyRegistered);
        }

        // `set_handler` only fails if the route is not a socket route; since we have already
        // checked that it is, this cannot fail.
        route
            .set_socket_handler(handler)
            .unwrap_or_else(|_| panic!("unexpected failure in set_socket_handler"));
        Ok(self)
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
            routes_by_path: self.routes_by_path,
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

#[cfg(test)]
mod test {
    use crate::{
        error::{Error, ServerError},
        socket::Connection,
        wait_for_server, App, StatusCode, Url, SERVER_STARTUP_RETRIES, SERVER_STARTUP_SLEEP_MS,
    };
    use async_std::{sync::RwLock, task::spawn};
    use async_tungstenite::{
        async_std::connect_async,
        tungstenite::{
            client::IntoClientRequest, http::header::*, protocol::frame::coding::CloseCode,
            protocol::Message,
        },
    };
    use futures::{
        stream::{iter, once, repeat},
        FutureExt, SinkExt, StreamExt,
    };
    use portpicker::pick_unused_port;
    use toml::toml;

    #[async_std::test]
    async fn test_socket_endpoint() {
        let mut app = App::<_, ServerError>::with_state(RwLock::new(()));
        let api_toml = toml! {
            [meta]
            FORMAT_VERSION = "0.1.0"

            [route.echo]
            PATH = ["/echo"]
            METHOD = "SOCKET"

            [route.once]
            PATH = ["/once"]
            METHOD = "SOCKET"

            [route.error]
            PATH = ["/error"]
            METHOD = "SOCKET"
        };
        {
            let mut api = app.module::<ServerError>("mod", api_toml).unwrap();
            api.socket(
                "echo",
                |_req, mut conn: Connection<String, String, _>, _state| {
                    async move {
                        while let Some(msg) = conn.next().await {
                            conn.send(&msg?).await?;
                        }
                        Ok(())
                    }
                    .boxed()
                },
            )
            .unwrap()
            .socket("once", |_req, mut conn: Connection<_, (), _>, _state| {
                async move {
                    conn.send("msg").boxed().await?;
                    Ok(())
                }
                .boxed()
            })
            .unwrap()
            .socket("error", |_req, _conn: Connection<(), (), _>, _state| {
                async move {
                    Err(ServerError::catch_all(
                        StatusCode::InternalServerError,
                        "an error message".to_string(),
                    ))
                }
                .boxed()
            })
            .unwrap();
        }
        let port = pick_unused_port().unwrap();
        let url: Url = format!("http://0.0.0.0:{}", port).parse().unwrap();
        spawn(app.serve(url.to_string()));
        wait_for_server(&url, SERVER_STARTUP_RETRIES, SERVER_STARTUP_SLEEP_MS).await;

        let mut socket_url = url.join("mod/echo").unwrap();
        socket_url.set_scheme("ws").unwrap();

        // Create a client that accepts JSON messages.
        let mut socket_req = socket_url.clone().into_client_request().unwrap();
        socket_req
            .headers_mut()
            .insert(ACCEPT, "application/json".parse().unwrap());
        let mut conn = connect_async(socket_req).await.unwrap().0;

        // Send a JSON message.
        conn.send(Message::Text(serde_json::to_string("hello").unwrap()))
            .await
            .unwrap();
        assert_eq!(
            conn.next().await.unwrap().unwrap(),
            Message::Text(serde_json::to_string("hello").unwrap())
        );

        // Send a binary message.
        conn.send(Message::Binary(bincode::serialize("goodbye").unwrap()))
            .await
            .unwrap();
        assert_eq!(
            conn.next().await.unwrap().unwrap(),
            Message::Text(serde_json::to_string("goodbye").unwrap())
        );

        // Create a client that accepts binary messages.
        let mut socket_req = socket_url.into_client_request().unwrap();
        socket_req
            .headers_mut()
            .insert(ACCEPT, "application/octet-stream".parse().unwrap());
        let mut conn = connect_async(socket_req).await.unwrap().0;

        // Send a JSON message.
        conn.send(Message::Text(serde_json::to_string("hello").unwrap()))
            .await
            .unwrap();
        assert_eq!(
            conn.next().await.unwrap().unwrap(),
            Message::Binary(bincode::serialize("hello").unwrap())
        );

        // Send a binary message.
        conn.send(Message::Binary(bincode::serialize("goodbye").unwrap()))
            .await
            .unwrap();
        assert_eq!(
            conn.next().await.unwrap().unwrap(),
            Message::Binary(bincode::serialize("goodbye").unwrap())
        );

        // Test a stream that exits normally.
        let mut socket_url = url.join("mod/once").unwrap();
        socket_url.set_scheme("ws").unwrap();
        let mut conn = connect_async(socket_url).await.unwrap().0;
        assert_eq!(
            conn.next().await.unwrap().unwrap(),
            Message::Text(serde_json::to_string("msg").unwrap())
        );
        match conn.next().await.unwrap().unwrap() {
            Message::Close(None) => {}
            msg => panic!("expected normal close frame, got {:?}", msg),
        };
        assert!(conn.next().await.is_none());

        // Test a stream that errors.
        let mut socket_url = url.join("mod/error").unwrap();
        socket_url.set_scheme("ws").unwrap();
        let mut conn = connect_async(socket_url).await.unwrap().0;
        match conn.next().await.unwrap().unwrap() {
            Message::Close(Some(frame)) => {
                assert_eq!(frame.code, CloseCode::Error);
                assert_eq!(frame.reason, "Error 500: an error message");
            }
            msg => panic!("expected error close frame, got {:?}", msg),
        }
        assert!(conn.next().await.is_none());
    }

    #[async_std::test]
    async fn test_stream_endpoint() {
        let mut app = App::<_, ServerError>::with_state(RwLock::new(()));
        let api_toml = toml! {
            [meta]
            FORMAT_VERSION = "0.1.0"

            [route.nat]
            PATH = ["/nat"]
            METHOD = "SOCKET"

            [route.once]
            PATH = ["/once"]
            METHOD = "SOCKET"

            [route.error]
            PATH = ["/error"]
            METHOD = "SOCKET"
        };
        {
            let mut api = app.module::<ServerError>("mod", api_toml).unwrap();
            api.stream("nat", |_req, _state| iter(0..).map(Ok).boxed())
                .unwrap()
                .stream("once", |_req, _state| once(async { Ok(0) }).boxed())
                .unwrap()
                .stream::<_, ()>("error", |_req, _state| {
                    // We intentionally return a stream that never terminates, to check that simply
                    // yielding an error causes the connection to terminate.
                    repeat(Err(ServerError::catch_all(
                        StatusCode::InternalServerError,
                        "an error message".to_string(),
                    )))
                    .boxed()
                })
                .unwrap();
        }
        let port = pick_unused_port().unwrap();
        let url: Url = format!("http://0.0.0.0:{}", port).parse().unwrap();
        spawn(app.serve(url.to_string()));
        wait_for_server(&url, SERVER_STARTUP_RETRIES, SERVER_STARTUP_SLEEP_MS).await;

        // Consume the `nat` stream.
        let mut socket_url = url.join("mod/nat").unwrap();
        socket_url.set_scheme("ws").unwrap();
        let mut conn = connect_async(socket_url).await.unwrap().0;

        for i in 0..100 {
            assert_eq!(
                conn.next().await.unwrap().unwrap(),
                Message::Text(serde_json::to_string(&i).unwrap())
            );
        }

        // Test a finite stream.
        let mut socket_url = url.join("mod/once").unwrap();
        socket_url.set_scheme("ws").unwrap();
        let mut conn = connect_async(socket_url).await.unwrap().0;

        assert_eq!(
            conn.next().await.unwrap().unwrap(),
            Message::Text(serde_json::to_string(&0).unwrap())
        );
        match conn.next().await.unwrap().unwrap() {
            Message::Close(None) => {}
            msg => panic!("expected normal close frame, got {:?}", msg),
        }
        assert!(conn.next().await.is_none());

        // Test a stream that errors.
        let mut socket_url = url.join("mod/error").unwrap();
        socket_url.set_scheme("ws").unwrap();
        let mut conn = connect_async(socket_url).await.unwrap().0;

        match conn.next().await.unwrap().unwrap() {
            Message::Close(Some(frame)) => {
                assert_eq!(frame.code, CloseCode::Error);
                assert_eq!(frame.reason, "Error 500: an error message");
            }
            msg => panic!("expected error close frame, got {:?}", msg),
        }
        assert!(conn.next().await.is_none());
    }
}
