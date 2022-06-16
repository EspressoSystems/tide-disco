use crate::{
    request::RequestParams,
    route::{Route, RouteParseError},
};
use futures::Future;
use serde::Serialize;
use snafu::{OptionExt, ResultExt, Snafu};
use std::collections::hash_map::{HashMap, IntoValues, Values};
use std::ops::Index;

/// An error encountered when parsing or constructing an [Api].
#[derive(Clone, Debug, Snafu)]
pub enum ApiError {
    Route { source: RouteParseError },
    MissingRoutesTable,
    RoutesMustBeTable,
    UndefinedRoute,
    HandlerAlreadyRegistered,
}

/// A description of an API.
///
/// An [Api] is a structured representation of an `api.toml` specification. It contains API-level
/// metadata and descriptions of all of the routes in the specification. It can be parsed from a
/// TOML file and registered as a module of an [App](crate::App).
pub struct Api<State, Error> {
    routes: HashMap<String, Route<State, Error>>,
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
        Ok(Self {
            routes: routes
                .into_iter()
                .map(|(name, spec)| {
                    let route = Route::new(name.clone(), spec).context(RouteSnafu)?;
                    Ok((route.name(), route))
                })
                .collect::<Result<_, _>>()?,
        })
    }

    /// Register a handler for a route.
    ///
    /// When the server receives a request whose URL matches the pattern of the route `name`,
    /// `handler` will be invoked with the parameters of the request and the result will be
    /// serialized into a response.
    ///
    /// If the route `name` does not exist in the API specification, or if the route already has a
    /// handler registered, an error is returned. Note that all routes are initialized with a
    /// default handler that echoes parameters and shows documentation, but this default handler can
    /// replaced by this function without raising [ApiError::HandlerAlreadyRegistered].
    pub fn at<F, Fut, T>(&mut self, name: &str, handler: F) -> Result<&mut Self, ApiError>
    where
        F: 'static + Send + Sync + Fn(RequestParams<State>) -> Fut,
        Fut: 'static + Send + Sync + Future<Output = Result<T, Error>>,
        T: 'static + Send + Sync + Serialize,
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
        }
    }
}
