// Copyright (c) 2022 Espresso Systems (espressosys.com)
// This file is part of the tide-disco library.

// You should have received a copy of the MIT License
// along with the tide-disco library. If not, see <https://mit-license.org/>.

use crate::{
    healthcheck::{HealthCheck, HealthStatus},
    method::{Method, ReadState, WriteState},
    metrics::Metrics,
    middleware::{error_handler, ErrorHandler},
    request::RequestParams,
    route::{self, *},
    socket, Html,
};
use async_std::sync::Arc;
use async_trait::async_trait;
use derivative::Derivative;
use futures::{
    future::{BoxFuture, FutureExt},
    stream::BoxStream,
};
use maud::{html, PreEscaped};
use semver::Version;
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use serde_with::{serde_as, DisplayFromStr};
use snafu::{OptionExt, ResultExt, Snafu};
use std::{
    borrow::Cow,
    collections::hash_map::{Entry, HashMap, IntoValues, Values},
    convert::Infallible,
    fmt::Display,
    fs,
    marker::PhantomData,
    ops::Index,
    path::{Path, PathBuf},
};
use tide::http::content::Accept;
use vbs::version::StaticVersionType;

/// An error encountered when parsing or constructing an [Api].
#[derive(Clone, Debug, Snafu, PartialEq, Eq)]
pub enum ApiError {
    Route { source: RouteParseError },
    ApiMustBeTable,
    MissingRoutesTable,
    RoutesMustBeTable,
    UndefinedRoute,
    HandlerAlreadyRegistered,
    IncorrectMethod { expected: Method, actual: Method },
    InvalidMetaTable { source: toml::de::Error },
    MissingFormatVersion,
    InvalidFormatVersion,
    AmbiguousRoutes { route1: String, route2: String },
    CannotReadToml { reason: String },
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

/// Metadata used for describing and documenting an API.
///
/// [ApiMetadata] contains version information about the API, as well as optional HTML fragments to
/// customize the formatting of automatically generated API documentation. Each of the supported
/// HTML fragments is optional and will be filled in with a reasonable default if not provided. Some
/// of the HTML fragments may contain "placeholders", which are identifiers enclosed in `{{ }}`,
/// like `{{SOME_PLACEHOLDER}}`. These will be replaced by contextual information when the
/// documentation is generated. The placeholders supported by each HTML fragment are documented
/// below.
#[serde_as]
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub struct ApiMetadata {
    /// The name of this API.
    ///
    /// Note that the name of the API may be overridden if the API is registered with an app using
    /// a different name.
    #[serde(default = "meta_defaults::name")]
    pub name: String,

    /// A description of this API.
    #[serde(default = "meta_defaults::description")]
    pub description: String,

    /// The version of the Tide Disco API specification format.
    ///
    /// If not specified, the version of this crate will be used.
    #[serde_as(as = "DisplayFromStr")]
    #[serde(default = "meta_defaults::format_version")]
    pub format_version: Version,

    /// HTML to be prepended to automatically generated documentation.
    ///
    /// # Placeholders
    ///
    /// * `NAME`: the name of the API
    /// * `DESCRIPTION`: the description provided in `Cargo.toml`
    /// * `VERSION`: the version of the API
    /// * `FORMAT_VERSION`: the `FORMAT_VERSION` of the API
    /// * `PUBLIC`: the URL where the public directory for this API is being served
    #[serde(default = "meta_defaults::html_top")]
    pub html_top: String,

    /// HTML to be appended to automatically generated documentation.
    #[serde(default = "meta_defaults::html_bottom")]
    pub html_bottom: String,

    /// The heading for documentation of a route.
    ///
    /// # Placeholders
    ///
    /// * `METHOD`: the method of the route
    /// * `NAME`: the name of the route
    #[serde(default = "meta_defaults::heading_entry")]
    pub heading_entry: String,

    /// The heading preceding documentation of all routes in this API.
    #[serde(default = "meta_defaults::heading_routes")]
    pub heading_routes: String,

    /// The heading preceding documentation of route parameters.
    #[serde(default = "meta_defaults::heading_parameters")]
    pub heading_parameters: String,

    /// The heading preceding documentation of a route description.
    #[serde(default = "meta_defaults::heading_description")]
    pub heading_description: String,

    /// HTML formatting the path of a route.
    ///
    /// # Placeholders
    ///
    /// * `PATH`: the path being formatted
    #[serde(default = "meta_defaults::route_path")]
    pub route_path: String,

    /// HTML preceding the contents of a table documenting the parameters of a route.
    #[serde(default = "meta_defaults::parameter_table_open")]
    pub parameter_table_open: String,

    /// HTML closing a table documenting the parameters of a route.
    #[serde(default = "meta_defaults::parameter_table_close")]
    pub parameter_table_close: String,

    /// HTML formatting an entry in a table documenting the parameters of a route.
    ///
    /// # Placeholders
    ///
    /// * `NAME`: the parameter being documented
    /// * `TYPE`: the type of the parameter being documented
    #[serde(default = "meta_defaults::parameter_row")]
    pub parameter_row: String,

    /// Documentation to insert in the parameters section of a route with no parameters.
    #[serde(default = "meta_defaults::parameter_none")]
    pub parameter_none: String,
}

impl Default for ApiMetadata {
    fn default() -> Self {
        // Deserialize an empty table, using the `serde` defaults for every field.
        toml::Value::Table(Default::default()).try_into().unwrap()
    }
}

mod meta_defaults {
    use super::Version;

    pub fn name() -> String {
        "default-tide-disco-api".to_string()
    }

    pub fn description() -> String {
        "Default Tide Disco API".to_string()
    }

    pub fn format_version() -> Version {
        "0.1.0".parse().unwrap()
    }

    pub fn html_top() -> String {
        "
        <!DOCTYPE html>
        <html lang='en'>
          <head>
            <meta charset='utf-8'>
            <title>{{NAME}} Reference</title>
            <link rel='stylesheet' href='{{PUBLIC}}/css/style.css'>
            <script src='{{PUBLIC}}/js/script.js'></script>
            <link rel='icon' type='image/svg+xml'
             href='/public/favicon.svg'>
          </head>
          <body>
            <div><a href='/'><img src='{{PUBLIC}}/espressosys_logo.svg'
                      alt='Espresso Systems Logo'
                      /></a></div>
            <h1>{{NAME}} API {{VERSION}} Reference</h1>
            <p>{{SHORT_DESCRIPTION}}</p><br/>
            {{LONG_DESCRIPTION}}
        "
        .to_string()
    }

    pub fn html_bottom() -> String {
        "
            <h1>&nbsp;</h1>
            <p>Copyright © 2022 Espresso Systems. All rights reserved.</p>
          </body>
        </html>
        "
        .to_string()
    }

    pub fn heading_entry() -> String {
        "<a name='{{NAME}}'><h3 class='entry'><span class='meth'>{{METHOD}}</span> {{NAME}}</h3></a>\n".to_string()
    }

    pub fn heading_routes() -> String {
        "<h3>Routes</h3>\n".to_string()
    }
    pub fn heading_parameters() -> String {
        "<h3>Parameters</h3>\n".to_string()
    }
    pub fn heading_description() -> String {
        "<h3>Description</h3>\n".to_string()
    }

    pub fn route_path() -> String {
        "<p class='path'>{{PATH}}</p>\n".to_string()
    }

    pub fn parameter_table_open() -> String {
        "<table>\n".to_string()
    }
    pub fn parameter_table_close() -> String {
        "</table>\n\n".to_string()
    }
    pub fn parameter_row() -> String {
        "<tr><td class='parameter'>{{NAME}}</td><td class='type'>{{TYPE}}</td></tr>\n".to_string()
    }
    pub fn parameter_none() -> String {
        "<div class='meta'>None</div>".to_string()
    }
}

/// A description of an API.
///
/// An [Api] is a structured representation of an `api.toml` specification. It contains API-level
/// metadata and descriptions of all of the routes in the specification. It can be parsed from a
/// TOML file and registered as a module of an [App](crate::App).
#[derive(Derivative)]
#[derivative(Debug(bound = ""))]
pub struct Api<State, Error, VER> {
    inner: ApiInner<State, Error>,
    _version: PhantomData<VER>,
}

/// A version-erased description of an API.
///
/// This type contains all the details of the API, with the version of the binary serialization
/// format type-erased and encapsulated into the route handlers. This type is used internally by
/// [`App`], to allow dynamic registration of different versions of APIs with different versions of
/// the binary format.
///
/// It is exposed publicly and manipulated _only_ via [`Api`], which wraps this type with a static
/// format version type parameter, which provides compile-time enforcement of format version
/// consistency as the API is being constructed, until it is registered with an [`App`] and
/// type-erased.
#[derive(Derivative)]
#[derivative(Debug(bound = ""))]
pub(crate) struct ApiInner<State, Error> {
    meta: Arc<ApiMetadata>,
    name: String,
    routes: HashMap<String, Route<State, Error>>,
    routes_by_path: HashMap<String, Vec<String>>,
    #[derivative(Debug = "ignore")]
    health_check: HealthCheckHandler<State>,
    api_version: Option<Version>,
    /// Error handler encapsulating the serialization format version for errors.
    ///
    /// This field is optional so it can be bound late, potentially after a `map_err` changes the
    /// error type. However, it will always be set after `Api::into_inner` is called.
    #[derivative(Debug = "ignore")]
    error_handler: Option<Arc<dyn ErrorHandler<Error>>>,
    /// Response handler encapsulating the serialization format version for version requests
    #[derivative(Debug = "ignore")]
    version_handler: Arc<dyn VersionHandler>,
    public: Option<PathBuf>,
    short_description: String,
    long_description: String,
}

pub(crate) trait VersionHandler:
    Send + Sync + Fn(&Accept, ApiVersion) -> Result<tide::Response, RouteError<Infallible>>
{
}
impl<F> VersionHandler for F where
    F: Send + Sync + Fn(&Accept, ApiVersion) -> Result<tide::Response, RouteError<Infallible>>
{
}

impl<'a, State, Error> IntoIterator for &'a ApiInner<State, Error> {
    type Item = &'a Route<State, Error>;
    type IntoIter = Values<'a, String, Route<State, Error>>;

    fn into_iter(self) -> Self::IntoIter {
        self.routes.values()
    }
}

impl<State, Error> IntoIterator for ApiInner<State, Error> {
    type Item = Route<State, Error>;
    type IntoIter = IntoValues<String, Route<State, Error>>;

    fn into_iter(self) -> Self::IntoIter {
        self.routes.into_values()
    }
}

impl<State, Error> Index<&str> for ApiInner<State, Error> {
    type Output = Route<State, Error>;

    fn index(&self, index: &str) -> &Route<State, Error> {
        &self.routes[index]
    }
}

/// Iterator for [routes_by_path](ApiInner::routes_by_path).
///
/// This type iterates over all of the routes that have a given path.
/// [routes_by_path](ApiInner::routes_by_path), in turn, returns an iterator over paths whose items
/// contain a [RoutesWithPath] iterator.
pub(crate) struct RoutesWithPath<'a, State, Error> {
    routes: std::slice::Iter<'a, String>,
    api: &'a ApiInner<State, Error>,
}

impl<'a, State, Error> Iterator for RoutesWithPath<'a, State, Error> {
    type Item = &'a Route<State, Error>;

    fn next(&mut self) -> Option<Self::Item> {
        Some(&self.api.routes[self.routes.next()?])
    }
}

impl<State, Error> ApiInner<State, Error> {
    /// Iterate over groups of routes with the same path.
    pub(crate) fn routes_by_path(
        &self,
    ) -> impl Iterator<Item = (&str, RoutesWithPath<'_, State, Error>)> {
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

    /// Check the health status of a server with the given state.
    pub(crate) async fn health(&self, req: RequestParams, state: &State) -> tide::Response {
        (self.health_check)(req, state).await
    }

    /// Get the version of this API.
    pub(crate) fn version(&self) -> ApiVersion {
        ApiVersion {
            api_version: self.api_version.clone(),
            spec_version: self.meta.format_version.clone(),
        }
    }

    pub(crate) fn public(&self) -> Option<&PathBuf> {
        self.public.as_ref()
    }

    pub(crate) fn set_name(&mut self, name: String) {
        self.name = name;
    }

    /// Compose an HTML page documenting all the routes in this API.
    pub(crate) fn documentation(&self) -> Html {
        html! {
            (PreEscaped(self.meta.html_top
                .replace("{{NAME}}", &self.name)
                .replace("{{SHORT_DESCRIPTION}}", &self.short_description)
                .replace("{{LONG_DESCRIPTION}}", &self.long_description)
                .replace("{{VERSION}}", &match &self.api_version {
                    Some(version) => version.to_string(),
                    None => "(no version)".to_string(),
                })
                .replace("{{FORMAT_VERSION}}", &self.meta.format_version.to_string())
                .replace("{{PUBLIC}}", &format!("/public/{}", self.name))))
            @for route in self.routes.values() {
                (route.documentation())
            }
            (PreEscaped(&self.meta.html_bottom))
        }
    }

    /// The short description of this API from the specification.
    pub(crate) fn short_description(&self) -> &str {
        &self.short_description
    }

    pub(crate) fn error_handler(&self) -> Arc<dyn ErrorHandler<Error>> {
        self.error_handler.clone().unwrap()
    }

    pub(crate) fn version_handler(&self) -> Arc<dyn VersionHandler> {
        self.version_handler.clone()
    }
}

impl<State, Error, VER> Api<State, Error, VER>
where
    State: 'static,
    Error: 'static,
    VER: StaticVersionType + 'static,
{
    /// Parse an API from a TOML specification.
    pub fn new(api: impl Into<toml::Value>) -> Result<Self, ApiError> {
        let mut api = api.into();
        let meta = match api
            .as_table_mut()
            .context(ApiMustBeTableSnafu)?
            .remove("meta")
        {
            Some(meta) => toml::Value::try_into(meta)
                .map_err(|source| ApiError::InvalidMetaTable { source })?,
            None => ApiMetadata::default(),
        };
        let meta = Arc::new(meta);
        let routes = match api.get("route") {
            Some(routes) => routes.as_table().context(RoutesMustBeTableSnafu)?,
            None => return Err(ApiError::MissingRoutesTable),
        };
        // Collect routes into a [HashMap] indexed by route name.
        let routes = routes
            .into_iter()
            .map(|(name, spec)| {
                let route = Route::new(name.clone(), spec, meta.clone()).context(RouteSnafu)?;
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

        // Parse description: the first line is a short description, to display when briefly
        // describing this API in a list. The rest is the long description, to display on this API's
        // own documentation page. Both are rendered to HTML via Markdown.
        let blocks = markdown::tokenize(&meta.description);
        let (short_description, long_description) = match blocks.split_first() {
            Some((short, long)) => {
                let render = |blocks| markdown::to_html(&markdown::generate_markdown(blocks));

                let short = render(vec![short.clone()]);
                let long = render(long.to_vec());

                // The short description is only one block, and sometimes we would like to display
                // it inline (as a `span`). Markdown automatically wraps blocks in `<p>`. We will
                // strip this outer tag so that we can wrap it in either `<p>` or `<span>`,
                // depending on the context.
                let short = short.strip_prefix("<p>").unwrap_or(&short);
                let short = short.strip_suffix("</p>").unwrap_or(short);
                let short = short.to_string();

                (short, long)
            }
            None => Default::default(),
        };

        Ok(Self {
            inner: ApiInner {
                name: meta.name.clone(),
                meta,
                routes,
                routes_by_path,
                health_check: Box::new(Self::default_health_check),
                api_version: None,
                error_handler: None,
                version_handler: Arc::new(|accept, version| {
                    respond_with(accept, version, VER::instance())
                }),
                public: None,
                short_description,
                long_description,
            },
            _version: Default::default(),
        })
    }

    /// Create an [Api] by reading a TOML specification from a file.
    pub fn from_file<P: AsRef<Path>>(path: P) -> Result<Self, ApiError> {
        let bytes = fs::read(path).map_err(|err| ApiError::CannotReadToml {
            reason: err.to_string(),
        })?;
        let string = std::str::from_utf8(&bytes).map_err(|err| ApiError::CannotReadToml {
            reason: err.to_string(),
        })?;
        Self::new(toml::from_str::<toml::Value>(string).map_err(|err| {
            ApiError::CannotReadToml {
                reason: err.to_string(),
            }
        })?)
    }

    /// Set the API version.
    ///
    /// The version information will automatically be included in responses to `GET /version`. This
    /// version can also be used to serve multiple major versions of the same API simultaneously,
    /// under a version prefix. For more information, see
    /// [App::register_module](crate::App::register_module).
    ///
    /// This is the version of the application or sub-application which this instance of [Api]
    /// represents. The versioning corresponds to the API specification passed to [new](Api::new),
    /// and may be different from the version of the Rust crate implementing the route handlers for
    /// the API.
    pub fn with_version(&mut self, version: Version) -> &mut Self {
        self.inner.api_version = Some(version);
        self
    }

    /// Serve the contents of `dir` at the URL `/public/{{NAME}}`.
    pub fn with_public(&mut self, dir: PathBuf) -> &mut Self {
        self.inner.public = Some(dir);
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
    /// # use vbs::version::StaticVersion;
    ///
    /// type State = u64;
    /// type StaticVer01 = StaticVersion<0, 1>;
    ///
    /// # fn ex(api: &mut Api<State, (), StaticVer01>) {
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
    /// # use vbs::version::StaticVersion;
    ///
    /// type State = Mutex<u64>;
    /// type StaticVer01 = StaticVersion<0, 1>;
    ///
    /// # fn ex(api: &mut Api<State, (), StaticVer01>) {
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
        VER: 'static + Send + Sync,
    {
        let route = self
            .inner
            .routes
            .get_mut(name)
            .ok_or(ApiError::UndefinedRoute)?;
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
            .set_fn_handler(handler, VER::instance())
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
        VER: 'static + Send + Sync + StaticVersionType,
    {
        assert!(method.is_http() && !method.is_mutable());
        let route = self
            .inner
            .routes
            .get_mut(name)
            .ok_or(ApiError::UndefinedRoute)?;
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
            .set_handler(ReadHandler::<_, VER>::from(handler))
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
    /// # use vbs::{Serializer, version::StaticVersion};
    ///
    /// type State = RwLock<u64>;
    /// type StaticVer01 = StaticVersion<0, 1>;
    ///
    /// # fn ex(api: &mut Api<State, (), StaticVer01>) {
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
        VER: 'static + Send + Sync,
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
        VER: 'static + Send + Sync,
    {
        assert!(method.is_http() && method.is_mutable());
        let route = self
            .inner
            .routes
            .get_mut(name)
            .ok_or(ApiError::UndefinedRoute)?;
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
            .set_handler(WriteHandler::<_, VER>::from(handler))
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
    /// # use vbs::version::StaticVersion;
    ///
    /// type State = RwLock<u64>;
    /// type StaticVer01 = StaticVersion<0, 1>;
    ///
    /// # fn ex(api: &mut Api<State, (), StaticVer01>) {
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
        VER: 'static + Send + Sync,
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
    /// # use vbs::version::StaticVersion;
    ///
    /// type State = RwLock<u64>;
    /// type StaticVer01 = StaticVersion<0, 1>;
    ///
    /// # fn ex(api: &mut Api<State, tide_disco::RequestError, StaticVer01>) {
    /// api.post("replace", |req, state| async move {
    ///     *state = req.integer_param("new_state")?;
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
        VER: 'static + Send + Sync,
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
    /// # use vbs::version::StaticVersion;
    ///
    /// type State = RwLock<Option<u64>>;
    /// type StaticVer01 = StaticVersion<0, 1>;
    ///
    /// # fn ex(api: &mut Api<State, (), StaticVer01>) {
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
        VER: 'static + Send + Sync,
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
    /// # use vbs::version::StaticVersion;
    ///
    /// # fn ex(api: &mut Api<(), ServerError, StaticVersion<0, 1>>) {
    /// api.socket("sum", |_req, mut conn: Connection<i32, i32, ServerError, StaticVersion<0, 1>>, _state| async move {
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
                socket::Connection<ToClient, FromClient, Error, VER>,
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
        VER: 'static + Send + Sync,
    {
        self.register_socket_handler(name, socket::stream_handler::<_, _, _, _, VER>(handler))
    }

    fn register_socket_handler(
        &mut self,
        name: &str,
        handler: socket::Handler<State, Error>,
    ) -> Result<&mut Self, ApiError> {
        let route = self
            .inner
            .routes
            .get_mut(name)
            .ok_or(ApiError::UndefinedRoute)?;
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

    /// Register a handler for a METRICS route.
    ///
    /// When the server receives any request whose URL matches the pattern for this route and whose
    /// headers indicate it is a request for metrics, the server will invoke this `handler` instead
    /// of the regular HTTP handler for the endpoint. Instead of returning a typed object to
    /// serialize, `handler` will return a [Metrics] object which will be serialized to plaintext
    /// using the Prometheus format.
    ///
    /// A request is considered a request for metrics, for the purpose of dispatching to this
    /// handler, if the method is GET and the `Accept` header specifies `text/plain` as a better
    /// response type than `application/json` and `application/octet-stream` (other Tide Disco
    /// handlers respond to the content types `application/json` or `application/octet-stream`). As
    /// a special case, a request with no `Accept` header or `Accept: *` will return metrics when
    /// there is a metrics route matching the request URL, since metrics are given priority over
    /// other content types when multiple routes match the URL.
    ///
    /// # Examples
    ///
    /// A metrics endpoint which keeps track of how many times it has been called.
    ///
    /// `api.toml`
    ///
    /// ```toml
    /// [route.metrics]
    /// PATH = ["/metrics"]
    /// METHOD = "METRICS"
    /// DOC = "Export Prometheus metrics."
    /// ```
    ///
    /// ```
    /// # use async_std::sync::Mutex;
    /// # use futures::FutureExt;
    /// # use tide_disco::{api::{Api, ApiError}, error::ServerError};
    /// # use std::borrow::Cow;
    /// # use vbs::version::StaticVersion;
    /// use prometheus::{Counter, Registry};
    ///
    /// struct State {
    ///     counter: Counter,
    ///     metrics: Registry,
    /// }
    /// type StaticVer01 = StaticVersion<0, 1>;
    ///
    /// # fn ex(_api: Api<Mutex<State>, ServerError, StaticVer01>) -> Result<(), ApiError> {
    /// let mut api: Api<Mutex<State>, ServerError, StaticVer01>;
    /// # api = _api;
    /// api.metrics("metrics", |_req, state| async move {
    ///     state.counter.inc();
    ///     Ok(Cow::Borrowed(&state.metrics))
    /// }.boxed())?;
    /// # Ok(())
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
    /// If the route `name` exists, but the method is not METRICS (that is, `METHOD = "M"` was used
    /// in the route definition in `api.toml`, with `M` other than `METRICS`) the error
    /// [IncorrectMethod](ApiError::IncorrectMethod) is returned.
    ///
    /// # Limitations
    ///
    /// [Like many function parameters](crate#boxed-futures) in [tide_disco](crate), the
    /// handler function is required to return a [BoxFuture].
    pub fn metrics<F, T>(&mut self, name: &str, handler: F) -> Result<&mut Self, ApiError>
    where
        F: 'static
            + Send
            + Sync
            + Fn(RequestParams, &State::State) -> BoxFuture<Result<Cow<T>, Error>>,
        T: 'static + Clone + Metrics,
        State: 'static + Send + Sync + ReadState,
        Error: 'static,
        VER: 'static + Send + Sync,
    {
        let route = self
            .inner
            .routes
            .get_mut(name)
            .ok_or(ApiError::UndefinedRoute)?;
        if route.method() != Method::Metrics {
            return Err(ApiError::IncorrectMethod {
                expected: Method::Metrics,
                actual: route.method(),
            });
        }
        if route.has_handler() {
            return Err(ApiError::HandlerAlreadyRegistered);
        }
        // `set_metrics_handler` only fails if the route is not a metrics route; since we have
        // already checked that it is, this cannot fail.
        route
            .set_metrics_handler(handler)
            .unwrap_or_else(|_| panic!("unexpected failure in set_metrics_handler"));
        Ok(self)
    }

    /// Set the health check handler for this API.
    ///
    /// This overrides the existing handler. If `health_check` has not yet been called, the default
    /// handler is one which simply returns `Health::default()`.
    pub fn with_health_check<H>(
        &mut self,
        handler: impl 'static + Send + Sync + Fn(&State) -> BoxFuture<H>,
    ) -> &mut Self
    where
        State: 'static + Send + Sync,
        H: 'static + HealthCheck,
        VER: 'static + Send + Sync,
    {
        self.inner.health_check = route::health_check_handler::<_, _, VER>(handler);
        self
    }

    /// Create a new [Api] which is just like this one, except has a transformed `Error` type.
    pub(crate) fn map_err<Error2>(
        self,
        f: impl 'static + Clone + Send + Sync + Fn(Error) -> Error2,
    ) -> Api<State, Error2, VER>
    where
        Error: 'static + Send + Sync,
        Error2: 'static,
        State: 'static + Send + Sync,
    {
        Api {
            inner: ApiInner {
                meta: self.inner.meta,
                name: self.inner.name,
                routes: self
                    .inner
                    .routes
                    .into_iter()
                    .map(|(name, route)| (name, route.map_err(f.clone())))
                    .collect(),
                routes_by_path: self.inner.routes_by_path,
                health_check: self.inner.health_check,
                api_version: self.inner.api_version,
                error_handler: None,
                version_handler: self.inner.version_handler,
                public: self.inner.public,
                short_description: self.inner.short_description,
                long_description: self.inner.long_description,
            },
            _version: Default::default(),
        }
    }

    pub(crate) fn into_inner(mut self) -> ApiInner<State, Error>
    where
        Error: crate::Error,
    {
        // This `into_inner` finalizes the error type for the API. At this point, ensure
        // `error_handler` is set.
        self.inner.error_handler = Some(error_handler::<Error, VER>());
        self.inner
    }

    fn default_health_check(req: RequestParams, _state: &State) -> BoxFuture<tide::Response> {
        async move {
            // If there is no healthcheck handler registered, just return [HealthStatus::Available]
            // by default; after all, if this handler is getting hit at all, the service must be up.
            route::health_check_response::<_, VER>(
                &req.accept().unwrap_or_else(|_| {
                    // The healthcheck endpoint is not allowed to fail, so just use the default
                    // content type if we can't parse the Accept header.
                    let mut accept = Accept::new();
                    accept.set_wildcard(true);
                    accept
                }),
                HealthStatus::Available,
            )
        }
        .boxed()
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
struct ReadHandler<F, VER> {
    handler: F,
    _version: PhantomData<VER>,
}

impl<F, VER> From<F> for ReadHandler<F, VER> {
    fn from(f: F) -> Self {
        Self {
            handler: f,
            _version: Default::default(),
        }
    }
}

#[async_trait]
impl<State, Error, F, R, VER> Handler<State, Error> for ReadHandler<F, VER>
where
    F: 'static
        + Send
        + Sync
        + Fn(RequestParams, &<State as ReadState>::State) -> BoxFuture<'_, Result<R, Error>>,
    R: Serialize,
    State: 'static + Send + Sync + ReadState,
    VER: 'static + Send + Sync + StaticVersionType,
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
            VER::instance(),
        )
    }
}

// A manual closure that serves a similar purpose as [ReadHandler].
struct WriteHandler<F, VER> {
    handler: F,
    _version: PhantomData<VER>,
}

impl<F, VER> From<F> for WriteHandler<F, VER> {
    fn from(f: F) -> Self {
        Self {
            handler: f,
            _version: Default::default(),
        }
    }
}

#[async_trait]
impl<State, Error, F, R, VER> Handler<State, Error> for WriteHandler<F, VER>
where
    F: 'static
        + Send
        + Sync
        + Fn(RequestParams, &mut <State as ReadState>::State) -> BoxFuture<'_, Result<R, Error>>,
    R: Serialize,
    State: 'static + Send + Sync + WriteState,
    VER: 'static + Send + Sync + StaticVersionType,
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
            VER::instance(),
        )
    }
}

#[cfg(test)]
mod test {
    use crate::{
        error::{Error, ServerError},
        healthcheck::HealthStatus,
        socket::Connection,
        testing::{setup_test, test_ws_client, test_ws_client_with_headers, Client},
        App, StatusCode, Url,
    };
    use async_std::{sync::RwLock, task::spawn};
    use async_tungstenite::{
        tungstenite::{http::header::*, protocol::frame::coding::CloseCode, protocol::Message},
        WebSocketStream,
    };
    use futures::{
        stream::{iter, once, repeat},
        AsyncRead, AsyncWrite, FutureExt, SinkExt, StreamExt,
    };
    use portpicker::pick_unused_port;
    use prometheus::{Counter, Registry};
    use std::borrow::Cow;
    use toml::toml;
    use vbs::{
        version::{StaticVersion, StaticVersionType},
        BinarySerializer, Serializer,
    };

    #[cfg(windows)]
    use async_tungstenite::tungstenite::Error as WsError;
    #[cfg(windows)]
    use std::io::ErrorKind;

    type StaticVer01 = StaticVersion<0, 1>;
    type SerializerV01 = Serializer<StaticVersion<0, 1>>;

    async fn check_stream_closed<S>(mut conn: WebSocketStream<S>)
    where
        S: AsyncRead + AsyncWrite + Unpin,
    {
        let msg = conn.next().await;

        #[cfg(not(windows))]
        assert!(msg.is_none(), "{:?}", msg);

        // Windows doesn't handle shutdown very gracefully.
        #[cfg(windows)]
        match msg {
            None => {}
            Some(Err(WsError::Io(err))) if err.kind() == ErrorKind::ConnectionAborted => {}
            msg => panic!(
                "expected end of stream or ConnectionAborted error, got {:?}",
                msg
            ),
        }
    }

    #[async_std::test]
    async fn test_socket_endpoint() {
        setup_test();

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
            let mut api = app
                .module::<ServerError, StaticVer01>("mod", api_toml)
                .unwrap();
            api.socket(
                "echo",
                |_req, mut conn: Connection<String, String, _, StaticVer01>, _state| {
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
            .socket(
                "once",
                |_req, mut conn: Connection<_, (), _, StaticVer01>, _state| {
                    async move {
                        conn.send("msg").boxed().await?;
                        Ok(())
                    }
                    .boxed()
                },
            )
            .unwrap()
            .socket(
                "error",
                |_req, _conn: Connection<(), (), _, StaticVer01>, _state| {
                    async move {
                        Err(ServerError::catch_all(
                            StatusCode::INTERNAL_SERVER_ERROR,
                            "an error message".to_string(),
                        ))
                    }
                    .boxed()
                },
            )
            .unwrap();
        }
        let port = pick_unused_port().unwrap();
        let url: Url = format!("http://localhost:{}", port).parse().unwrap();
        spawn(app.serve(format!("0.0.0.0:{}", port), StaticVer01::instance()));

        // Create a client that accepts JSON messages.
        let mut conn = test_ws_client_with_headers(
            url.join("mod/echo").unwrap(),
            &[(ACCEPT, "application/json")],
        )
        .await;

        // Send a JSON message.
        conn.send(Message::Text(serde_json::to_string("hello").unwrap()))
            .await
            .unwrap();
        assert_eq!(
            conn.next().await.unwrap().unwrap(),
            Message::Text(serde_json::to_string("hello").unwrap())
        );

        // Send a binary message.
        conn.send(Message::Binary(
            SerializerV01::serialize("goodbye").unwrap(),
        ))
        .await
        .unwrap();
        assert_eq!(
            conn.next().await.unwrap().unwrap(),
            Message::Text(serde_json::to_string("goodbye").unwrap())
        );

        // Create a client that accepts binary messages.
        let mut conn = test_ws_client_with_headers(
            url.join("mod/echo").unwrap(),
            &[(ACCEPT, "application/octet-stream")],
        )
        .await;

        // Send a JSON message.
        conn.send(Message::Text(serde_json::to_string("hello").unwrap()))
            .await
            .unwrap();
        assert_eq!(
            conn.next().await.unwrap().unwrap(),
            Message::Binary(SerializerV01::serialize("hello").unwrap())
        );

        // Send a binary message.
        conn.send(Message::Binary(
            SerializerV01::serialize("goodbye").unwrap(),
        ))
        .await
        .unwrap();
        assert_eq!(
            conn.next().await.unwrap().unwrap(),
            Message::Binary(SerializerV01::serialize("goodbye").unwrap())
        );

        // Test a stream that exits normally.
        let mut conn = test_ws_client(url.join("mod/once").unwrap()).await;
        assert_eq!(
            conn.next().await.unwrap().unwrap(),
            Message::Text(serde_json::to_string("msg").unwrap())
        );
        match conn.next().await.unwrap().unwrap() {
            Message::Close(None) => {}
            msg => panic!("expected normal close frame, got {:?}", msg),
        };
        check_stream_closed(conn).await;

        // Test a stream that errors.
        let mut conn = test_ws_client(url.join("mod/error").unwrap()).await;
        match conn.next().await.unwrap().unwrap() {
            Message::Close(Some(frame)) => {
                assert_eq!(frame.code, CloseCode::Error);
                assert_eq!(frame.reason, "Error 500: an error message");
            }
            msg => panic!("expected error close frame, got {:?}", msg),
        }
        check_stream_closed(conn).await;
    }

    #[async_std::test]
    async fn test_stream_endpoint() {
        setup_test();

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
            let mut api = app
                .module::<ServerError, StaticVer01>("mod", api_toml)
                .unwrap();
            api.stream("nat", |_req, _state| iter(0..).map(Ok).boxed())
                .unwrap()
                .stream("once", |_req, _state| once(async { Ok(0) }).boxed())
                .unwrap()
                .stream::<_, ()>("error", |_req, _state| {
                    // We intentionally return a stream that never terminates, to check that simply
                    // yielding an error causes the connection to terminate.
                    repeat(Err(ServerError::catch_all(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "an error message".to_string(),
                    )))
                    .boxed()
                })
                .unwrap();
        }
        let port = pick_unused_port().unwrap();
        let url: Url = format!("http://localhost:{}", port).parse().unwrap();
        spawn(app.serve(format!("0.0.0.0:{}", port), StaticVer01::instance()));

        // Consume the `nat` stream.
        let mut conn = test_ws_client(url.join("mod/nat").unwrap()).await;
        for i in 0..100 {
            assert_eq!(
                conn.next().await.unwrap().unwrap(),
                Message::Text(serde_json::to_string(&i).unwrap())
            );
        }

        // Test a finite stream.
        let mut conn = test_ws_client(url.join("mod/once").unwrap()).await;
        assert_eq!(
            conn.next().await.unwrap().unwrap(),
            Message::Text(serde_json::to_string(&0).unwrap())
        );
        match conn.next().await.unwrap().unwrap() {
            Message::Close(None) => {}
            msg => panic!("expected normal close frame, got {:?}", msg),
        }
        check_stream_closed(conn).await;

        // Test a stream that errors.
        let mut conn = test_ws_client(url.join("mod/error").unwrap()).await;
        match conn.next().await.unwrap().unwrap() {
            Message::Close(Some(frame)) => {
                assert_eq!(frame.code, CloseCode::Error);
                assert_eq!(frame.reason, "Error 500: an error message");
            }
            msg => panic!("expected error close frame, got {:?}", msg),
        }
        check_stream_closed(conn).await;
    }

    #[async_std::test]
    async fn test_custom_healthcheck() {
        setup_test();

        let mut app = App::<_, ServerError>::with_state(HealthStatus::Available);
        let api_toml = toml! {
            [meta]
            FORMAT_VERSION = "0.1.0"

            [route.dummy]
            PATH = ["/dummy"]
        };
        {
            let mut api = app
                .module::<ServerError, StaticVer01>("mod", api_toml)
                .unwrap();
            api.with_health_check(|state| async move { *state }.boxed());
        }
        let port = pick_unused_port().unwrap();
        let url: Url = format!("http://localhost:{}", port).parse().unwrap();
        spawn(app.serve(format!("0.0.0.0:{}", port), StaticVer01::instance()));
        let client = Client::new(url).await;

        let res = client.get("/mod/healthcheck").send().await.unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(
            res.json::<HealthStatus>().await.unwrap(),
            HealthStatus::Available
        );
    }

    #[async_std::test]
    async fn test_metrics_endpoint() {
        setup_test();

        struct State {
            metrics: Registry,
            counter: Counter,
        }

        let counter = Counter::new(
            "counter",
            "count of how many times metrics have been exported",
        )
        .unwrap();
        let metrics = Registry::new();
        metrics.register(Box::new(counter.clone())).unwrap();
        let state = State { metrics, counter };

        let mut app = App::<_, ServerError>::with_state(RwLock::new(state));
        let api_toml = toml! {
            [meta]
            FORMAT_VERSION = "0.1.0"

            [route.metrics]
            PATH = ["/metrics"]
            METHOD = "METRICS"
        };
        {
            let mut api = app
                .module::<ServerError, StaticVer01>("mod", api_toml)
                .unwrap();
            api.metrics("metrics", |_req, state| {
                async move {
                    state.counter.inc();
                    Ok(Cow::Borrowed(&state.metrics))
                }
                .boxed()
            })
            .unwrap();
        }
        let port = pick_unused_port().unwrap();
        let url: Url = format!("http://localhost:{port}").parse().unwrap();
        spawn(app.serve(format!("0.0.0.0:{port}"), StaticVer01::instance()));
        let client = Client::new(url).await;

        for i in 1..5 {
            tracing::info!("making metrics request {i}");
            let expected = format!("# HELP counter count of how many times metrics have been exported\n# TYPE counter counter\ncounter {i}\n");
            let res = client.get("mod/metrics").send().await.unwrap();
            assert_eq!(res.status(), StatusCode::OK);
            assert_eq!(res.text().await.unwrap(), expected);
        }
    }
}
