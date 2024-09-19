// Copyright (c) 2022 Espresso Systems (espressosys.com)
// This file is part of the tide-disco library.

// You should have received a copy of the MIT License
// along with the tide-disco library. If not, see <https://mit-license.org/>.

use crate::{
    api::{Api, ApiError, ApiInner, ApiVersion},
    dispatch::{self, DispatchError, Trie},
    healthcheck::{HealthCheck, HealthStatus},
    http,
    method::Method,
    middleware::{request_params, AddErrorBody, MetricsMiddleware},
    request::RequestParams,
    route::{health_check_response, respond_with, Handler, Route, RouteError},
    socket::SocketError,
    Html, StatusCode,
};
use async_std::sync::Arc;
use derive_more::From;
use futures::future::{BoxFuture, FutureExt};
use include_dir::{include_dir, Dir};
use lazy_static::lazy_static;
use maud::{html, PreEscaped};
use semver::Version;
use serde::{Deserialize, Serialize};
use serde_with::{serde_as, DisplayFromStr};
use snafu::{ResultExt, Snafu};
use std::{
    collections::btree_map::BTreeMap,
    convert::Infallible,
    env,
    fmt::Display,
    fs, io,
    ops::{Deref, DerefMut},
    path::PathBuf,
};
use tide::{
    http::headers::HeaderValue,
    security::{CorsMiddleware, Origin},
};
use tide_websockets::WebSocket;
use vbs::version::StaticVersionType;

pub use tide::listener::{Listener, ToListener};

/// A tide-disco server application.
///
/// An [App] is a collection of API modules, plus a global `State`. Modules can be registered by
/// constructing an [Api] for each module and calling [App::register_module]. Once all of the
/// desired modules are registered, the app can be converted into an asynchronous server task using
/// [App::serve].
///
/// Note that the [`App`] is bound to a binary serialization version `VER`. This format only applies
/// to application-level endpoints like `/version` and `/healthcheck`. The binary format version in
/// use by any given API module may differ, depending on the supported version of the API.
#[derive(Debug)]
pub struct App<State, Error> {
    pub(crate) modules: Trie<ApiInner<State, Error>>,
    pub(crate) state: Arc<State>,
    app_version: Option<Version>,
}

/// An error encountered while building an [App].
#[derive(Clone, Debug, From, Snafu, PartialEq, Eq)]
pub enum AppError {
    Api { source: ApiError },
    Dispatch { source: DispatchError },
}

impl<State: Send + Sync + 'static, Error: 'static> App<State, Error> {
    /// Create a new [App] with a given state.
    pub fn with_state(state: State) -> Self {
        Self {
            modules: Default::default(),
            state: Arc::new(state),
            app_version: None,
        }
    }

    /// Create and register an API module.
    ///
    /// Creates a new [`Api`] with the given `api` specification and returns an RAII guard for this
    /// API. The guard can be used to access the API module, configure it, and populate its
    /// handlers. When [`Module::register`] is called on the guard (or the guard is dropped), the
    /// module will be registered in this [`App`] as if by calling
    /// [`register_module`](Self::register_module).
    pub fn module<'a, ModuleError, ModuleVersion>(
        &'a mut self,
        base_url: &'a str,
        api: impl Into<toml::Value>,
    ) -> Result<Module<'a, State, Error, ModuleError, ModuleVersion>, AppError>
    where
        Error: crate::Error + From<ModuleError>,
        ModuleError: Send + Sync + 'static,
        ModuleVersion: StaticVersionType + 'static,
    {
        Ok(Module {
            app: self,
            base_url,
            api: Some(Api::new(api).context(ApiSnafu)?),
        })
    }

    /// Register an API module.
    ///
    /// The module `api` will be registered as an implementation of the module hosted under the URL
    /// prefix `base_url`.
    ///
    /// # Versioning
    ///
    /// Multiple versions of the same [`Api`] may be registered by calling this function several
    /// times with the same `base_url`, and passing in different APIs which must have different
    /// _major_ versions. The API version can be set using [`Api::with_version`].
    ///
    /// When multiple versions of the same API are registered, requests for endpoints directly under
    /// the base URL, like `GET /base_url/endpoint`, will always be dispatched to the latest
    /// available version of the API. There will in addition be an extension of `base_url` for each
    /// major version registered, so `GET /base_url/v1/endpoint` will always dispatch to the
    /// `endpoint` handler in the module with major version 1, if it exists, regardless of what the
    /// latest version is.
    ///
    /// It is an error to register multiple versions of the same module with the same major version.
    /// It is _not_ an error to register non-sequential versions of a module. For example, you could
    /// have `/base_url/v2` and `/base_url/v4`, but not `v1` or `v3`. Requests for `v1` or `v3` will
    /// simply fail.
    ///
    /// The intention of this functionality is to allow for non-disruptive breaking updates. Rather
    /// than deploying a new major version of the API with breaking changes _in place of_ the old
    /// version, breaking all your clients, you can continue to serve the old version for some
    /// period of time under a version prefix. Clients can point at this version prefix until they
    /// update their software to use the new version, on their own time.
    ///
    /// Note that non-breaking changes (e.g. new endpoints) can be deployed in place of an existing
    /// API without even incrementing the major version. The need for serving two versions of an API
    /// simultaneously only arises when you have breaking changes.
    pub fn register_module<ModuleError, ModuleVersion>(
        &mut self,
        base_url: &str,
        api: Api<State, ModuleError, ModuleVersion>,
    ) -> Result<&mut Self, AppError>
    where
        Error: crate::Error + From<ModuleError>,
        ModuleError: Send + Sync + 'static,
        ModuleVersion: StaticVersionType + 'static,
    {
        let mut api = api.map_err(Error::from).into_inner();
        api.set_name(base_url.to_string());

        let major_version = match api.version().api_version {
            Some(version) => version.major,
            None => {
                // If no version is explicitly specified, default to 0.
                0
            }
        };

        self.modules
            .insert(dispatch::split(base_url), major_version, api)?;
        Ok(self)
    }

    /// Set the application version.
    ///
    /// The version information will automatically be included in responses to `GET /version`.
    ///
    /// This is the version of the overall application, which may encompass several APIs, each with
    /// their own version. Changes to the version of any of the APIs which make up this application
    /// should imply a change to the application version, but the application version may also
    /// change without changing any of the API versions.
    ///
    /// This version is optional, as the `/version` endpoint will automatically include the version
    /// of each registered API, which is usually enough to uniquely identify the application. Set
    /// this explicitly if you want to track the version of additional behavior or interfaces which
    /// are not encompassed by the sub-modules of this application.
    ///
    /// If you set an application version, it is a good idea to use the version of the application
    /// crate found in Cargo.toml. This can be automatically found at build time using the
    /// environment variable `CARGO_PKG_VERSION` and the [env!] macro. As long as the following code
    /// is contained in the application crate, it should result in a reasonable version:
    ///
    /// ```
    /// # use vbs::version::StaticVersion;
    /// # type StaticVer01 = StaticVersion<0, 1>;
    /// # fn ex(app: &mut tide_disco::App<(), ()>) {
    /// app.with_version(env!("CARGO_PKG_VERSION").parse().unwrap());
    /// # }
    /// ```
    pub fn with_version(&mut self, version: Version) -> &mut Self {
        self.app_version = Some(version);
        self
    }

    /// Get the version of this application.
    pub fn version(&self) -> AppVersion {
        AppVersion {
            app_version: self.app_version.clone(),
            disco_version: env!("CARGO_PKG_VERSION").parse().unwrap(),
            modules: self
                .modules
                .iter()
                .map(|module| {
                    (
                        module.path(),
                        module
                            .versions
                            .values()
                            .rev()
                            .map(|api| api.version())
                            .collect(),
                    )
                })
                .collect(),
        }
    }

    /// Check the health of each registered module in response to a request.
    ///
    /// The response includes a status code for each module, which will be [StatusCode::OK] if the
    /// module is healthy. Detailed health status from each module is not included in the response
    /// (due to type erasure) but can be queried using [module_health](Self::module_health) or by
    /// hitting the endpoint `GET /:module/healthcheck`.
    pub async fn health(&self, req: RequestParams, state: &State) -> AppHealth {
        let mut modules_health = BTreeMap::<String, BTreeMap<_, _>>::new();
        let mut status = HealthStatus::Available;
        for module in &self.modules {
            let versions_health = modules_health.entry(module.path()).or_default();
            for (version, api) in &module.versions {
                let health = StatusCode::from(api.health(req.clone(), state).await.status());
                if health != StatusCode::OK {
                    status = HealthStatus::Unhealthy;
                }
                versions_health.insert(*version, health);
            }
        }
        AppHealth {
            status,
            modules: modules_health,
        }
    }

    /// Check the health of the named module.
    ///
    /// The resulting [Response](tide::Response) has a status code which is [StatusCode::OK] if the
    /// module is healthy. The response body is constructed from the results of the module's
    /// registered healthcheck handler. If the module does not have an explicit healthcheck
    /// handler, the response will be a [HealthStatus].
    ///
    /// `major_version` can be used to query the health status of a specific version of the desired
    /// module. If it is not provided, the most recent supported version will be queried.
    ///
    /// If there is no module with the given name or version, returns [None].
    pub async fn module_health(
        &self,
        req: RequestParams,
        state: &State,
        module: &str,
        major_version: Option<u64>,
    ) -> Option<tide::Response> {
        let module = self.modules.get(dispatch::split(module))?;
        let api = match major_version {
            Some(v) => module.versions.get(&v)?,
            None => module.versions.last_key_value()?.1,
        };
        Some(api.health(req, state).await)
    }
}

static DEFAULT_PUBLIC_DIR: Dir<'_> = include_dir!("$CARGO_MANIFEST_DIR/public/media");
lazy_static! {
    static ref DEFAULT_PUBLIC_PATH: PathBuf = {
        // The contents of the default public directory are included in the binary. The first time
        // the default directory is used, if ever, we extract them to a directory on the host file
        // system and return the path to that directory.
        let path = PathBuf::from("$HOME/tide-disco/public/media");
        // If the path already exists, move it aside so we can update it.
        let _ = fs::rename(&path, path.with_extension("old"));
        DEFAULT_PUBLIC_DIR.extract(&path).unwrap();
        path
    };
}

impl<State, Error> App<State, Error>
where
    State: Send + Sync + 'static,
    Error: 'static + crate::Error,
{
    /// Serve the [App] asynchronously.
    ///
    /// `VER` controls the binary format version used for responses to top-level endpoints like
    /// `/version` and `/healthcheck`. All endpoints for specific API modules will use the format
    /// version of that module (`ModuleVersion` when the module was
    /// [registered](Self::register_module)).
    pub async fn serve<L, VER>(self, listener: L, bind_version: VER) -> io::Result<()>
    where
        L: ToListener<Arc<Self>>,
        VER: StaticVersionType + 'static,
    {
        let state = Arc::new(self);
        let mut server = tide::Server::with_state(state.clone());
        server.with(Self::version_middleware);
        server.with(AddErrorBody::<Error>::with_version::<VER>());
        server.with(
            CorsMiddleware::new()
                .allow_methods("GET, POST".parse::<HeaderValue>().unwrap())
                .allow_headers("*".parse::<HeaderValue>().unwrap())
                .allow_origin(Origin::from("*"))
                .allow_credentials(true),
        );

        for module in &state.modules {
            Self::register_api(&mut server, module.prefix.clone(), &module.versions)?;
        }

        // Register app-level routes summarizing the status and documentation of all the registered
        // modules. We skip this step if this is a singleton app with only one module registered at
        // the root URL, as these app-level endpoints would conflict with the (probably more
        // specific) API-level status endpoints.
        if !state.modules.is_singleton() {
            // Register app-level automatic routes: `healthcheck` and `version`.
            server
                .at("healthcheck")
                .get(move |req: tide::Request<Arc<Self>>| async move {
                    let state = req.state().clone();
                    let app_state = &*state.state;
                    let req = request_params(req, &[]).await?;
                    let accept = req.accept()?;
                    let res = state.health(req, app_state).await;
                    Ok(health_check_response::<_, VER>(&accept, res))
                });
            server
                .at("version")
                .get(move |req: tide::Request<Arc<Self>>| async move {
                    let accept = RequestParams::accept_from_headers(&req)?;
                    respond_with(&accept, req.state().version(), bind_version)
                        .map_err(|err| Error::from_route_error::<Infallible>(err).into_tide_error())
                });

            // Serve documentation at the root URL for discoverability
            server
                .at("/")
                .all(move |req: tide::Request<Arc<Self>>| async move {
                    Ok(tide::Response::from(Self::top_level_docs(req)))
                });
        }

        server.listen(listener).await
    }

    fn list_apis(&self) -> Html {
        html! {
            ul {
                @for module in &self.modules {
                    li {
                        // Link to the alias for the latest version as the primary link.
                        a href=(format!("/{}", module.path())) {(module.path())}
                        // Add a superscript link (link a footnote) for each specific supported
                        // version, linking to documentation for that specific version.
                        @for version in module.versions.keys().rev() {
                            sup {
                                a href=(format!("/v{version}/{}", module.path())) {
                                    (format!("[v{version}]"))
                                }
                            }
                        }
                        " "
                        // Take the description of the latest supported version.
                        (PreEscaped(module.versions.last_key_value().unwrap().1.short_description()))
                    }
                }
            }
        }
    }

    fn register_api(
        server: &mut tide::Server<Arc<Self>>,
        prefix: Vec<String>,
        versions: &BTreeMap<u64, ApiInner<State, Error>>,
    ) -> io::Result<()> {
        for (version, api) in versions {
            Self::register_api_version(server, &prefix, *version, api)?;
        }
        Ok(())
    }

    fn register_api_version(
        server: &mut tide::Server<Arc<Self>>,
        prefix: &[String],
        version: u64,
        api: &ApiInner<State, Error>,
    ) -> io::Result<()> {
        // Clippy complains if the only non-trivial operation in an `unwrap_or_else` closure is
        // a deref, but for `lazy_static` types, deref is an effectful operation that (in this
        // case) causes a directory to be renamed and another extracted. We only want to execute
        // this if we need to (if `api.public()` is `None`) so we disable the lint.
        #[allow(clippy::unnecessary_lazy_evaluations)]
        server
            .at("/public")
            .at(&format!("v{version}"))
            .at(&prefix.join("/"))
            .serve_dir(api.public().unwrap_or_else(|| &DEFAULT_PUBLIC_PATH))?;

        // Register routes for this API.
        let mut version_endpoint = server.at(&format!("/v{version}"));
        let mut api_endpoint = if prefix.is_empty() {
            version_endpoint
        } else {
            version_endpoint.at(&prefix.join("/"))
        };
        api_endpoint.with(AddErrorBody::new(api.error_handler()));
        for (path, routes) in api.routes_by_path() {
            let mut endpoint = api_endpoint.at(path);
            let routes = routes.collect::<Vec<_>>();

            // Register socket and metrics middlewares. These must be registered before any
            // regular HTTP routes, because Tide only applies middlewares to routes which were
            // already registered before the route handler.
            if let Some(socket_route) = routes.iter().find(|route| route.method() == Method::Socket)
            {
                // If there is a socket route with this pattern, add the socket middleware to
                // all endpoints registered under this pattern, so that any request with any
                // method that has the socket upgrade headers will trigger a WebSockets upgrade.
                Self::register_socket(prefix.to_vec(), version, &mut endpoint, socket_route);
            }
            if let Some(metrics_route) = routes
                .iter()
                .find(|route| route.method() == Method::Metrics)
            {
                // If there is a metrics route with this pattern, add the metrics middleware to
                // all endpoints registered under this pattern, so that a request to this path
                // with the right headers will return metrics instead of going through the
                // normal method-based dispatching.
                Self::register_metrics(prefix.to_vec(), version, &mut endpoint, metrics_route);
            }

            // Register the HTTP routes.
            for route in routes {
                if let Method::Http(method) = route.method() {
                    Self::register_route(prefix.to_vec(), version, &mut endpoint, route, method);
                }
            }
        }

        // Register automatic routes for this API: documentation, `healthcheck` and `version`. Serve
        // documentation at the root of the API (with or without a trailing slash).
        for path in ["", "/"] {
            let prefix = prefix.to_vec();
            api_endpoint
                .at(path)
                .all(move |req: tide::Request<Arc<Self>>| {
                    let prefix = prefix.clone();
                    async move {
                        let api = &req.state().clone().modules[&prefix].versions[&version];
                        Ok(api.documentation())
                    }
                });
        }
        {
            let prefix = prefix.to_vec();
            api_endpoint
                .at("*path")
                .all(move |req: tide::Request<Arc<Self>>| {
                    let prefix = prefix.clone();
                    async move {
                        // The request did not match any route. Serve documentation for the API.
                        let api = &req.state().clone().modules[&prefix].versions[&version];
                        let docs = html! {
                            "No route matches /" (req.param("path")?)
                            br{}
                            (api.documentation())
                        };
                        Ok(tide::Response::builder(StatusCode::NOT_FOUND)
                            .body(docs.into_string())
                            .build())
                    }
                });
        }
        {
            let prefix = prefix.to_vec();
            api_endpoint
                .at("healthcheck")
                .get(move |req: tide::Request<Arc<Self>>| {
                    let prefix = prefix.clone();
                    async move {
                        let api = &req.state().clone().modules[&prefix].versions[&version];
                        let state = req.state().clone();
                        Ok(api
                            .health(request_params(req, &[]).await?, &state.state)
                            .await)
                    }
                });
        }
        {
            let prefix = prefix.to_vec();
            api_endpoint
                .at("version")
                .get(move |req: tide::Request<Arc<Self>>| {
                    let prefix = prefix.clone();
                    async move {
                        let api = &req.state().modules[&prefix].versions[&version];
                        let accept = RequestParams::accept_from_headers(&req)?;
                        api.version_handler()(&accept, api.version())
                            .map_err(|err| Error::from_route_error(err).into_tide_error())
                    }
                });
        }

        Ok(())
    }

    fn register_route(
        api: Vec<String>,
        version: u64,
        endpoint: &mut tide::Route<Arc<Self>>,
        route: &Route<State, Error>,
        method: http::Method,
    ) {
        let name = route.name();
        endpoint.method(method, move |req: tide::Request<Arc<Self>>| {
            let name = name.clone();
            let api = api.clone();
            async move {
                let route = &req.state().clone().modules[&api].versions[&version][&name];
                let state = &*req.state().clone().state;
                let req = request_params(req, route.params()).await?;
                route
                    .handle(req, state)
                    .await
                    .map_err(|err| match err {
                        RouteError::AppSpecific(err) => err,
                        _ => Error::from_route_error(err),
                    })
                    .map_err(|err| err.into_tide_error())
            }
        });
    }

    fn register_metrics(
        api: Vec<String>,
        version: u64,
        endpoint: &mut tide::Route<Arc<Self>>,
        route: &Route<State, Error>,
    ) {
        let name = route.name();
        if route.has_handler() {
            // If there is a metrics handler, add middleware to the endpoint to intercept the
            // request and respond with metrics, rather than the usual HTTP dispatching, if the
            // appropriate headers are set.
            endpoint.with(MetricsMiddleware::new(name.clone(), api.clone(), version));
        }

        // Register a catch-all HTTP handler for the route, which serves the route documentation as
        // HTML. This ensures that there is at least one endpoint registered with the Tide
        // dispatcher, so that the middleware actually fires on requests to this path. In addition,
        // this handler will trigger for requests that are not otherwise valid, aiding in
        // discoverability.
        //
        // We register the default handler using `all`, which makes it act as a fallback handler.
        // This means if there are other, non-metrics routes with this same path, we will still
        // dispatch to them if the path is hit with the appropriate method.
        Self::register_fallback(api, version, endpoint, route);
    }

    fn register_socket(
        api: Vec<String>,
        version: u64,
        endpoint: &mut tide::Route<Arc<Self>>,
        route: &Route<State, Error>,
    ) {
        let name = route.name();
        if route.has_handler() {
            // If there is a socket handler, add the [WebSocket] middleware to the endpoint, so that
            // upgrade requests will automatically upgrade to a WebSockets connection.
            let name = name.clone();
            let api = api.clone();
            endpoint.with(WebSocket::new(
                move |req: tide::Request<Arc<Self>>, conn| {
                    let name = name.clone();
                    let api = api.clone();
                    async move {
                        let route = &req.state().clone().modules[&api].versions[&version][&name];
                        let state = &*req.state().clone().state;
                        let req = request_params(req, route.params()).await?;
                        route
                            .handle_socket(req, conn, state)
                            .await
                            .map_err(|err| match err {
                                SocketError::AppSpecific(err) => err,
                                _ => Error::from_socket_error(err),
                            })
                            .map_err(|err| err.into_tide_error())
                    }
                },
            ));
        }

        // Register a catch-all HTTP handler for the route, which serves the route documentation as
        // HTML. This ensures that there is at least one endpoint registered with the Tide
        // dispatcher, so that the middleware actually fires on requests to this path. In addition,
        // this handler will trigger for requests that are not valid WebSockets handshakes. The
        // documentation should make clear that this is a WebSockets endpoint, aiding in
        // discoverability. This will also trigger if there is no socket handler for this route,
        // which will signal to the developer that they need to implement a socket handler for this
        // route to work.
        //
        // We register the default handler using `all`, which makes it act as a fallback handler.
        // This means if there are other, non-socket routes with this same path, we will still
        // dispatch to them if the path is hit with the appropriate method.
        Self::register_fallback(api, version, endpoint, route);
    }

    fn register_fallback(
        api: Vec<String>,
        version: u64,
        endpoint: &mut tide::Route<Arc<Self>>,
        route: &Route<State, Error>,
    ) {
        let name = route.name();
        endpoint.all(move |req: tide::Request<Arc<Self>>| {
            let name = name.clone();
            let api = api.clone();
            async move {
                let route = &req.state().clone().modules[&api].versions[&version][&name];
                route
                    .default_handler()
                    .map_err(|err| match err {
                        RouteError::AppSpecific(err) => err,
                        _ => Error::from_route_error(err),
                    })
                    .map_err(|err| err.into_tide_error())
            }
        });
    }

    /// Server middleware which returns redirect responses for requests lacking an explicit version
    /// prefix.
    fn version_middleware(
        req: tide::Request<Arc<Self>>,
        next: tide::Next<Arc<Self>>,
    ) -> BoxFuture<tide::Result> {
        async move {
            let Some(path) = req.url().path_segments() else {
                // If we can't parse the path, we can't run this middleware. Do our best by
                // continuing the request processing lifecycle.
                return Ok(next.run(req).await);
            };
            let path = path.collect::<Vec<_>>();
            let Some(seg1) = path.first() else {
                // This is the root URL, with no path segments. Nothing for this middleware to do.
                return Ok(next.run(req).await);
            };
            if seg1.is_empty() {
                // This is the root URL, with no path segments. Nothing for this middleware to do.
                return Ok(next.run(req).await);
            }

            // The first segment is either a version identifier or (part of) an API identifier
            // (implicitly requesting the latest version of the API). We handle these cases
            // differently.
            if let Some(version) = seg1.strip_prefix('v').and_then(|n| n.parse().ok()) {
                // If the version identifier is present, we probably don't need a redirect. However,
                // we still check if this is a valid version for the request API. If not, we will
                // serve documentation listing the available versions.
                let Some(module) = req.state().modules.search(&path[1..]) else {
                    let message = format!("No API matches /{}", path[1..].join("/"));
                    return Ok(Self::top_level_error(req, StatusCode::NOT_FOUND, message));
                };
                if !module.versions.contains_key(&version) {
                    // This version is not supported, list suported versions.
                    return Ok(html! {
                        "Unsupported version v" (version) ". Supported versions are:"
                        ul {
                            @for v in module.versions.keys().rev() {
                                li {
                                    a href=(format!("/v{v}/{}", module.path())) { "v" (v) }
                                }
                            }
                        }
                    }
                    .into());
                }

                // This is a valid request with a specific version. It should be handled
                // successfully by the route handlers for this API.
                Ok(next.run(req).await)
            } else {
                // If the first path segment is not a version prefix, then the path is either the
                // name of an API (implicitly requesting the latest version) or one of the magic
                // top-level endpoints (version, healthcheck). Validate the API and then redirect.
                if !req.state().modules.is_singleton() && ["version", "healthcheck"].contains(seg1)
                {
                    return Ok(next.run(req).await);
                }
                let Some(module) = req.state().modules.search(&path) else {
                    let message = format!("No API matches /{}", path.join("/"));
                    return Ok(Self::top_level_error(req, StatusCode::NOT_FOUND, message));
                };

                let latest_version = *module.versions.last_key_value().unwrap().0;
                let path = path.join("/");
                Ok(tide::Redirect::permanent(format!("/v{latest_version}/{path}")).into())
            }
        }
        .boxed()
    }

    /// Top-level documentation about the app.
    fn top_level_docs(req: tide::Request<Arc<Self>>) -> PreEscaped<String> {
        html! {
            br {}
            "This is a Tide Disco app composed of the following modules:"
            (req.state().list_apis())
        }
    }

    /// Documentation served when there is a routing error at the app level.
    fn top_level_error(
        req: tide::Request<Arc<Self>>,
        status: StatusCode,
        message: impl Display,
    ) -> tide::Response {
        let docs = html! {
            (message.to_string())
            (Self::top_level_docs(req))
        };
        tide::Response::builder(status)
            .body(docs.into_string())
            .build()
    }
}

/// The health status of an application.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct AppHealth {
    /// The status of the overall application.
    ///
    /// [HealthStatus::Available] if all of the application's modules are healthy, otherwise a
    /// [HealthStatus] variant with [status](HealthCheck::status) other than 200.
    pub status: HealthStatus,
    /// The status of each registered module, indexed by version.
    pub modules: BTreeMap<String, BTreeMap<u64, StatusCode>>,
}

impl HealthCheck for AppHealth {
    fn status(&self) -> StatusCode {
        self.status.status()
    }
}

/// Version information about an application.
#[serde_as]
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct AppVersion {
    /// The supported versions of each module registered with this application.
    ///
    /// Versions for each module are ordered from newest to oldest.
    pub modules: BTreeMap<String, Vec<ApiVersion>>,

    /// The version of this application.
    #[serde_as(as = "Option<DisplayFromStr>")]
    pub app_version: Option<Version>,

    /// The version of the Tide Disco server framework.
    #[serde_as(as = "DisplayFromStr")]
    pub disco_version: Version,
}

/// RAII guard to ensure a module is registered after it is configured.
///
/// This type allows the owner to configure an [`Api`] module via the [`Deref`] and [`DerefMut`]
/// traits. Once the API is configured, this object can be dropped, which will automatically
/// register the module with the [`App`].
///
/// # Panics
///
/// Note that if anything goes wrong during module registration (for example, there is already an
/// incompatible module registered with the same name), the drop implementation may panic. To handle
/// errors without panicking, call [`register`](Self::register) explicitly.
#[derive(Debug)]
pub struct Module<'a, State, Error, ModuleError, ModuleVersion>
where
    State: Send + Sync + 'static,
    Error: crate::Error + From<ModuleError> + 'static,
    ModuleError: Send + Sync + 'static,
    ModuleVersion: StaticVersionType + 'static,
{
    app: &'a mut App<State, Error>,
    base_url: &'a str,
    // This is only an [Option] so we can [take] out of it during [drop].
    api: Option<Api<State, ModuleError, ModuleVersion>>,
}

impl<'a, State, Error, ModuleError, ModuleVersion> Deref
    for Module<'a, State, Error, ModuleError, ModuleVersion>
where
    State: Send + Sync + 'static,
    Error: crate::Error + From<ModuleError> + 'static,
    ModuleError: Send + Sync + 'static,
    ModuleVersion: StaticVersionType + 'static,
{
    type Target = Api<State, ModuleError, ModuleVersion>;

    fn deref(&self) -> &Self::Target {
        self.api.as_ref().unwrap()
    }
}

impl<'a, State, Error, ModuleError, ModuleVersion> DerefMut
    for Module<'a, State, Error, ModuleError, ModuleVersion>
where
    State: Send + Sync + 'static,
    Error: crate::Error + From<ModuleError> + 'static,
    ModuleError: Send + Sync + 'static,
    ModuleVersion: StaticVersionType + 'static,
{
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.api.as_mut().unwrap()
    }
}

impl<'a, State, Error, ModuleError, ModuleVersion> Drop
    for Module<'a, State, Error, ModuleError, ModuleVersion>
where
    State: Send + Sync + 'static,
    Error: crate::Error + From<ModuleError> + 'static,
    ModuleError: Send + Sync + 'static,
    ModuleVersion: StaticVersionType + 'static,
{
    fn drop(&mut self) {
        self.register_impl().unwrap();
    }
}

impl<'a, State, Error, ModuleError, ModuleVersion>
    Module<'a, State, Error, ModuleError, ModuleVersion>
where
    State: Send + Sync + 'static,
    Error: crate::Error + From<ModuleError> + 'static,
    ModuleError: Send + Sync + 'static,
    ModuleVersion: StaticVersionType + 'static,
{
    /// Register this module with the linked app.
    pub fn register(mut self) -> Result<(), AppError> {
        self.register_impl()
    }

    /// Perform the logic of [`Self::register`] without consuming `self`, so this can be called from
    /// `drop`.
    fn register_impl(&mut self) -> Result<(), AppError> {
        if let Some(api) = self.api.take() {
            self.app.register_module(self.base_url, api)?;
            Ok(())
        } else {
            // Already registered.
            Ok(())
        }
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::{
        error::{Error, ServerError},
        metrics::Metrics,
        socket::Connection,
        testing::{setup_test, test_ws_client, Client},
        Url,
    };
    use async_std::{sync::RwLock, task::spawn};
    use async_tungstenite::tungstenite::Message;
    use futures::{FutureExt, SinkExt, StreamExt};
    use portpicker::pick_unused_port;
    use serde::de::DeserializeOwned;
    use std::{borrow::Cow, fmt::Debug};
    use toml::toml;
    use vbs::{version::StaticVersion, BinarySerializer, Serializer};

    type StaticVer01 = StaticVersion<0, 1>;
    type SerializerV01 = Serializer<StaticVer01>;

    type StaticVer02 = StaticVersion<0, 2>;
    type SerializerV02 = Serializer<StaticVer02>;

    type StaticVer03 = StaticVersion<0, 3>;
    type SerializerV03 = Serializer<StaticVer03>;

    #[derive(Clone, Copy, Debug)]
    struct FakeMetrics;

    impl Metrics for FakeMetrics {
        type Error = ServerError;

        fn export(&self) -> Result<String, Self::Error> {
            Ok("METRICS".into())
        }
    }

    /// Test route dispatching for routes with the same path and different methods.
    #[async_std::test]
    async fn test_method_dispatch() {
        setup_test();

        use crate::http::Method::*;

        let mut app = App::<_, ServerError>::with_state(RwLock::new(FakeMetrics));
        let api_toml = toml! {
            [meta]
            FORMAT_VERSION = "0.1.0"

            [route.get_test]
            PATH = ["/test"]
            METHOD = "GET"

            [route.post_test]
            PATH = ["/test"]
            METHOD = "POST"

            [route.put_test]
            PATH = ["/test"]
            METHOD = "PUT"

            [route.delete_test]
            PATH = ["/test"]
            METHOD = "DELETE"

            [route.socket_test]
            PATH = ["/test"]
            METHOD = "SOCKET"

            [route.metrics_test]
            PATH = ["/test"]
            METHOD = "METRICS"
        };
        {
            let mut api = app
                .module::<ServerError, StaticVer01>("mod", api_toml)
                .unwrap();
            api.get("get_test", |_req, _state| {
                async move { Ok(Get.to_string()) }.boxed()
            })
            .unwrap()
            .post("post_test", |_req, _state| {
                async move { Ok(Post.to_string()) }.boxed()
            })
            .unwrap()
            .put("put_test", |_req, _state| {
                async move { Ok(Put.to_string()) }.boxed()
            })
            .unwrap()
            .delete("delete_test", |_req, _state| {
                async move { Ok(Delete.to_string()) }.boxed()
            })
            .unwrap()
            .socket(
                "socket_test",
                |_req, mut conn: Connection<str, (), _, StaticVer01>, _state| {
                    async move {
                        conn.send("SOCKET").await.unwrap();
                        Ok(())
                    }
                    .boxed()
                },
            )
            .unwrap()
            .metrics("metrics_test", |_req, state| {
                async move { Ok(Cow::Borrowed(state)) }.boxed()
            })
            .unwrap();
        }
        let port = pick_unused_port().unwrap();
        let url: Url = format!("http://localhost:{}", port).parse().unwrap();
        spawn(app.serve(format!("0.0.0.0:{}", port), StaticVer01::instance()));
        let client = Client::new(url.clone()).await;

        // Regular HTTP methods.
        for method in [Get, Post, Put, Delete] {
            let res = client
                .request(method, "mod/test")
                .header("Accept", "application/json")
                .send()
                .await
                .unwrap();
            assert_eq!(res.status(), StatusCode::OK);
            assert_eq!(res.json::<String>().await.unwrap(), method.to_string());
        }

        // Metrics with Accept header.
        let res = client
            .get("mod/test")
            .header("Accept", "text/plain")
            .send()
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(res.text().await.unwrap(), "METRICS");

        // Metrics without Accept header.
        let res = client.get("mod/test").send().await.unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(res.text().await.unwrap(), "METRICS");

        // Socket.
        let mut conn = test_ws_client(url.join("mod/test").unwrap()).await;
        let msg = conn.next().await.unwrap().unwrap();
        let body: String = match msg {
            Message::Text(m) => serde_json::from_str(&m).unwrap(),
            Message::Binary(m) => SerializerV01::deserialize(&m).unwrap(),
            m => panic!("expected Text or Binary message, but got {}", m),
        };
        assert_eq!(body, "SOCKET");
    }

    /// Test route dispatching for routes with patterns containing different parmaeters
    #[async_std::test]
    async fn test_param_dispatch() {
        setup_test();

        let mut app = App::<_, ServerError>::with_state(RwLock::new(()));
        let api_toml = toml! {
            [meta]
            FORMAT_VERSION = "0.1.0"

            [route.test]
            PATH = ["/test/a/:a", "/test/b/:b"]
            ":a" = "Integer"
            ":b" = "Boolean"
        };
        {
            let mut api = app
                .module::<ServerError, StaticVer01>("mod", api_toml)
                .unwrap();
            api.get("test", |req, _state| {
                async move {
                    if let Some(a) = req.opt_integer_param::<_, i32>("a")? {
                        Ok(("a", a.to_string()))
                    } else {
                        Ok(("b", req.boolean_param("b")?.to_string()))
                    }
                }
                .boxed()
            })
            .unwrap();
        }
        let port = pick_unused_port().unwrap();
        let url: Url = format!("http://localhost:{}", port).parse().unwrap();
        spawn(app.serve(format!("0.0.0.0:{}", port), StaticVer01::instance()));
        let client = Client::new(url.clone()).await;

        let res = client.get("mod/test/a/42").send().await.unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(
            res.json::<(String, String)>().await.unwrap(),
            ("a".to_string(), "42".to_string())
        );

        let res = client.get("mod/test/b/true").send().await.unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(
            res.json::<(String, String)>().await.unwrap(),
            ("b".to_string(), "true".to_string())
        );
    }

    #[async_std::test]
    async fn test_versions() {
        setup_test();

        let mut app = App::<_, ServerError>::with_state(RwLock::new(()));

        // Create two different, non-consecutive major versions of an API. One method will be
        // deleted in version 1, one will be added in version 3, and one will be present in both
        // versions (with a different implementation).
        let v1_toml = toml! {
            [meta]
            FORMAT_VERSION = "0.1.0"

            [route.deleted]
            PATH = ["/deleted"]

            [route.unchanged]
            PATH = ["/unchanged"]
        };
        let v3_toml = toml! {
            [meta]
            FORMAT_VERSION = "0.1.0"

            [route.added]
            PATH = ["/added"]

            [route.unchanged]
            PATH = ["/unchanged"]
        };

        {
            let mut v1 = app
                .module::<ServerError, StaticVer01>("mod", v1_toml.clone())
                .unwrap();
            v1.with_version("1.0.0".parse().unwrap())
                .get("deleted", |_req, _state| {
                    async move { Ok("deleted v1") }.boxed()
                })
                .unwrap()
                .get("unchanged", |_req, _state| {
                    async move { Ok("unchanged v1") }.boxed()
                })
                .unwrap()
                // Add a custom healthcheck for the old version so we can check healthcheck routing.
                .with_health_check(|_state| {
                    async move { HealthStatus::TemporarilyUnavailable }.boxed()
                });
        }
        {
            // Registering the same major version twice is an error.
            let mut api = app
                .module::<ServerError, StaticVer01>("mod", v1_toml)
                .unwrap();
            api.with_version("1.1.1".parse().unwrap());
            assert_eq!(
                api.register().unwrap_err(),
                DispatchError::ModuleAlreadyExists {
                    prefix: "mod".into(),
                    version: 1,
                }
                .into()
            );
        }
        {
            let mut v3 = app
                .module::<ServerError, StaticVer01>("mod", v3_toml.clone())
                .unwrap();
            v3.with_version("3.0.0".parse().unwrap())
                .get("added", |_req, _state| {
                    async move { Ok("added v3") }.boxed()
                })
                .unwrap()
                .get("unchanged", |_req, _state| {
                    async move { Ok("unchanged v3") }.boxed()
                })
                .unwrap();
        }

        let port = pick_unused_port().unwrap();
        let url: Url = format!("http://localhost:{}", port).parse().unwrap();
        spawn(app.serve(format!("0.0.0.0:{}", port), StaticVer01::instance()));
        let client = Client::new(url.clone()).await;

        // First check that we can call all the expected methods.
        assert_eq!(
            "deleted v1",
            client
                .get("v1/mod/deleted")
                .send()
                .await
                .unwrap()
                .json::<String>()
                .await
                .unwrap()
        );
        assert_eq!(
            "unchanged v1",
            client
                .get("v1/mod/unchanged")
                .send()
                .await
                .unwrap()
                .json::<String>()
                .await
                .unwrap()
        );
        // For the v3 methods, we can query with or without a version prefix.
        for prefix in ["", "/v3"] {
            let span = tracing::info_span!("version", prefix);
            let _enter = span.enter();

            assert_eq!(
                "added v3",
                client
                    .get(&format!("{prefix}/mod/added"))
                    .send()
                    .await
                    .unwrap()
                    .json::<String>()
                    .await
                    .unwrap()
            );
            assert_eq!(
                "unchanged v3",
                client
                    .get(&format!("{prefix}/mod/unchanged"))
                    .send()
                    .await
                    .unwrap()
                    .json::<String>()
                    .await
                    .unwrap()
            );
        }

        // Test documentation for invalid routes.
        let check_docs = |version, route: &'static str| {
            let client = &client;
            async move {
                let span = tracing::info_span!("check_docs", ?version, route);
                let _enter = span.enter();
                tracing::info!("test invalid route docs");

                let prefix = match version {
                    Some(v) => format!("/v{v}"),
                    None => "".into(),
                };

                // Invalid route or no route with no version prefix redirects to documentation for
                // the latest supported version.
                let version = version.unwrap_or(3);

                let res = client
                    .get(&format!("{prefix}/mod/{route}"))
                    .send()
                    .await
                    .unwrap();
                let docs = res.text().await.unwrap();
                if !route.is_empty() {
                    assert!(
                        docs.contains(&format!("No route matches /{route}")),
                        "{docs}"
                    );
                }
                assert!(
                    docs.contains(&format!("mod API {version}.0.0 Reference")),
                    "{docs}"
                );
            }
        };

        for route in ["", "deleted"] {
            check_docs(None, route).await;
        }
        for route in ["", "deleted"] {
            check_docs(Some(3), route).await;
        }
        for route in ["", "added"] {
            check_docs(Some(1), route).await;
        }

        // Request with an unsupported version lists the supported versions.
        let expected_html = html! {
            "Unsupported version v2. Supported versions are:"
            ul {
                li {
                    a href="/v3/mod" {"v3"}
                }
                li {
                    a href="/v1/mod" {"v1"}
                }
            }
        }
        .into_string();
        for route in ["", "/unchanged"] {
            let span = tracing::info_span!("unsupported_version_docs", route);
            let _enter = span.enter();
            tracing::info!("test unsupported version docs");

            let res = client.get(&format!("/v2/mod{route}")).send().await.unwrap();
            let docs = res.text().await.unwrap();
            assert_eq!(docs, expected_html);
        }

        // Test version endpoints.
        for version in [None, Some(1), Some(3)] {
            let span = tracing::info_span!("version_endpoints", version);
            let _enter = span.enter();
            tracing::info!("test version endpoints");

            let prefix = match version {
                Some(v) => format!("/v{v}"),
                None => "".into(),
            };
            let res = client
                .get(&format!("{prefix}/mod/version"))
                .send()
                .await
                .unwrap();
            assert_eq!(
                res.json::<ApiVersion>()
                    .await
                    .unwrap()
                    .api_version
                    .unwrap()
                    .major,
                version.unwrap_or(3)
            );
        }

        // Test the application version.
        let res = client.get("version").send().await.unwrap();
        assert_eq!(
            res.json::<AppVersion>().await.unwrap().modules["mod"],
            [
                ApiVersion {
                    api_version: Some("3.0.0".parse().unwrap()),
                    spec_version: "0.1.0".parse().unwrap(),
                },
                ApiVersion {
                    api_version: Some("1.0.0".parse().unwrap()),
                    spec_version: "0.1.0".parse().unwrap(),
                }
            ]
        );

        // Test healthcheck endpoints.
        for version in [None, Some(1), Some(3)] {
            let span = tracing::info_span!("healthcheck_endpoints", version);
            let _enter = span.enter();
            tracing::info!("test healthcheck endpoints");

            let prefix = match version {
                Some(v) => format!("/v{v}"),
                None => "".into(),
            };
            let res = client
                .get(&format!("{prefix}/mod/healthcheck"))
                .send()
                .await
                .unwrap();
            let status = res.status();
            let health: HealthStatus = res.json().await.unwrap();
            assert_eq!(health.status(), status);
            assert_eq!(
                health,
                if version == Some(1) {
                    HealthStatus::TemporarilyUnavailable
                } else {
                    HealthStatus::Available
                }
            );
        }

        // Test the application health.
        let res = client.get("healthcheck").send().await.unwrap();
        assert_eq!(res.status(), StatusCode::SERVICE_UNAVAILABLE);
        let health: AppHealth = res.json().await.unwrap();
        assert_eq!(health.status, HealthStatus::Unhealthy);
        assert_eq!(
            health.modules["mod"],
            [(3, StatusCode::OK), (1, StatusCode::SERVICE_UNAVAILABLE)].into()
        );
    }

    #[async_std::test]
    async fn test_api_disco() {
        setup_test();

        // Test discoverability documentation when a request is for an unknown API.
        let mut app = App::<_, ServerError>::with_state(());
        app.module::<ServerError, StaticVer01>(
            "the-correct-module",
            toml! {
                route = {}
            },
        )
        .unwrap()
        .with_version("1.0.0".parse().unwrap());

        let port = pick_unused_port().unwrap();
        let url: Url = format!("http://localhost:{}", port).parse().unwrap();
        spawn(app.serve(format!("0.0.0.0:{}", port), StaticVer01::instance()));
        let client = Client::new(url.clone()).await;

        let expected_list_item = html! {
            a href="/the-correct-module" {"the-correct-module"}
            sup {
                a href="/v1/the-correct-module" {"[v1]"}
            }
        }
        .into_string();

        for version_prefix in ["", "/v1"] {
            let docs = client
                .get(&format!("{version_prefix}/test"))
                .send()
                .await
                .unwrap()
                .text()
                .await
                .unwrap();
            assert!(docs.contains("No API matches /test"), "{docs}");
            assert!(docs.contains(&expected_list_item), "{docs}");
        }

        // Top level documentation.
        let docs = client.get("").send().await.unwrap().text().await.unwrap();
        assert!(!docs.contains("No API matches"), "{docs}");
        assert!(docs.contains(&expected_list_item), "{docs}");

        let docs = client
            .get("/v1")
            .send()
            .await
            .unwrap()
            .text()
            .await
            .unwrap();
        assert!(docs.contains("No API matches /"), "{docs}");
        assert!(docs.contains(&expected_list_item), "{docs}");
    }

    #[async_std::test]
    async fn test_post_redirect_idempotency() {
        setup_test();

        let mut app = App::<_, ServerError>::with_state(RwLock::new(0));

        let api_toml = toml! {
            [meta]
            FORMAT_VERSION = "0.1.0"

            [route.test]
            METHOD = "POST"
            PATH = ["/test"]
        };
        {
            let mut api = app
                .module::<ServerError, StaticVer01>("mod", api_toml.clone())
                .unwrap();
            api.post("test", |_req, state| {
                async move {
                    *state += 1;
                    Ok(*state)
                }
                .boxed()
            })
            .unwrap();
        }

        let port = pick_unused_port().unwrap();
        let url: Url = format!("http://localhost:{}", port).parse().unwrap();
        spawn(app.serve(format!("0.0.0.0:{}", port), StaticVer01::instance()));
        let client = Client::new(url.clone()).await;

        for i in 1..3 {
            // Request gets redirected to latest version of API and resent, but endpoint handler
            // only executes once.
            assert_eq!(
                client
                    .post("mod/test")
                    .send()
                    .await
                    .unwrap()
                    .json::<u64>()
                    .await
                    .unwrap(),
                i
            );
        }
    }

    #[async_std::test]
    async fn test_format_versions() {
        setup_test();

        // Register two modules with different binary format versions, each in turn different from
        // the app-level version. Each module has two endpoints, one which always succeeds and one
        // which always fails, so we can test error serialization.
        let mut app = App::<_, ServerError>::with_state(());
        let api_toml = toml! {
            [meta]
            FORMAT_VERSION = "0.1.0"

            [route.ok]
            METHOD = "GET"
            PATH = ["/ok"]

            [route.err]
            METHOD = "GET"
            PATH = ["/err"]
        };

        fn init_api<VER: StaticVersionType + 'static>(api: &mut Api<(), ServerError, VER>) {
            api.get("ok", |_req, _state| async move { Ok("ok") }.boxed())
                .unwrap()
                .get("err", |_req, _state| {
                    async move {
                        Err::<String, _>(ServerError::catch_all(
                            StatusCode::INTERNAL_SERVER_ERROR,
                            "err".into(),
                        ))
                    }
                    .boxed()
                })
                .unwrap();
        }

        {
            let mut api = app
                .module::<ServerError, StaticVer02>("mod02", api_toml.clone())
                .unwrap();
            init_api(&mut api);
        }
        {
            let mut api = app
                .module::<ServerError, StaticVer03>("mod03", api_toml.clone())
                .unwrap();
            init_api(&mut api);
        }

        let port = pick_unused_port().unwrap();
        let url: Url = format!("http://localhost:{}", port).parse().unwrap();
        spawn(app.serve(format!("0.0.0.0:{}", port), StaticVer01::instance()));
        let client = Client::new(url.clone()).await;

        async fn get<S: BinarySerializer, T: DeserializeOwned>(
            client: &Client,
            endpoint: &str,
            expected_status: StatusCode,
        ) -> anyhow::Result<T> {
            tracing::info!("GET {endpoint} ->");
            let res = client
                .get(endpoint)
                .header("Accept", "application/octet-stream")
                .send()
                .await
                .unwrap();
            tracing::info!(?res, "<-");
            assert_eq!(res.status(), expected_status);
            let bytes = res.bytes().await.unwrap();
            anyhow::Context::context(
                S::deserialize(&bytes),
                format!("failed to deserialize bytes {bytes:?}"),
            )
        }

        #[tracing::instrument(skip(client))]
        async fn check_ok<S: BinarySerializer>(
            client: &Client,
            endpoint: &str,
            expected: impl Debug + DeserializeOwned + Eq,
        ) {
            tracing::info!("checking successful deserialization");
            assert_eq!(
                expected,
                get::<S, _>(client, endpoint, StatusCode::OK).await.unwrap()
            );
        }

        let api_version = ApiVersion {
            spec_version: "0.1.0".parse().unwrap(),
            api_version: None,
        };

        check_ok::<SerializerV01>(
            &client,
            "healthcheck",
            AppHealth {
                status: HealthStatus::Available,
                modules: [
                    ("mod02".into(), [(0, StatusCode::OK)].into()),
                    ("mod03".into(), [(0, StatusCode::OK)].into()),
                ]
                .into(),
            },
        )
        .await;
        check_ok::<SerializerV01>(
            &client,
            "version",
            AppVersion {
                app_version: None,
                disco_version: env!("CARGO_PKG_VERSION").parse().unwrap(),
                modules: [
                    ("mod02".into(), vec![api_version.clone()]),
                    ("mod03".into(), vec![api_version.clone()]),
                ]
                .into(),
            },
        )
        .await;
        check_ok::<SerializerV02>(&client, "mod02/ok", "ok".to_string()).await;
        check_ok::<SerializerV02>(&client, "mod02/healthcheck", HealthStatus::Available).await;
        check_ok::<SerializerV02>(&client, "mod02/version", api_version.clone()).await;
        check_ok::<SerializerV03>(&client, "mod03/ok", "ok".to_string()).await;
        check_ok::<SerializerV03>(&client, "mod03/healthcheck", HealthStatus::Available).await;
        check_ok::<SerializerV03>(&client, "mod03/version", api_version.clone()).await;

        #[tracing::instrument(skip(client))]
        async fn check_wrong_version<S: BinarySerializer, T: Debug + DeserializeOwned>(
            client: &Client,
            endpoint: &str,
        ) {
            tracing::info!("checking deserialization fails with wrong version");
            get::<S, T>(client, endpoint, StatusCode::OK)
                .await
                .unwrap_err();
        }

        check_wrong_version::<SerializerV02, AppHealth>(&client, "healthcheck").await;
        check_wrong_version::<SerializerV02, AppVersion>(&client, "version").await;
        check_wrong_version::<SerializerV03, String>(&client, "mod02/ok").await;
        check_wrong_version::<SerializerV03, HealthStatus>(&client, "mod02/healthcheck").await;
        check_wrong_version::<SerializerV03, ApiVersion>(&client, "mod02/version").await;
        check_wrong_version::<SerializerV01, String>(&client, "mod03/ok").await;
        check_wrong_version::<SerializerV01, HealthStatus>(&client, "mod03/healthcheck").await;
        check_wrong_version::<SerializerV01, ApiVersion>(&client, "mod03/version").await;

        #[tracing::instrument(skip(client))]
        async fn check_err<S: BinarySerializer>(client: &Client, endpoint: &str) {
            tracing::info!("checking error deserialization");
            tracing::info!("checking successful deserialization");
            assert_eq!(
                get::<S, ServerError>(client, endpoint, StatusCode::INTERNAL_SERVER_ERROR)
                    .await
                    .unwrap(),
                ServerError::catch_all(StatusCode::INTERNAL_SERVER_ERROR, "err".into())
            );
        }

        check_err::<SerializerV02>(&client, "mod02/err").await;
        check_err::<SerializerV03>(&client, "mod03/err").await;
    }

    #[async_std::test]
    async fn test_api_prefix() {
        setup_test();

        // It is illegal to register two API modules where one is a prefix (in terms of route
        // segments) of another.
        for (api1, api2) in [
            ("", "api"),
            ("api", ""),
            ("path", "path/sub"),
            ("path/sub", "path"),
        ] {
            tracing::info!(api1, api2, "test case");
            let (prefix, conflict) = if api1.len() < api2.len() {
                (api1.to_string(), api2.to_string())
            } else {
                (api2.to_string(), api1.to_string())
            };

            let mut app = App::<_, ServerError>::with_state(());
            let toml = toml! {
                route = {}
            };
            app.module::<ServerError, StaticVer01>(api1, toml.clone())
                .unwrap()
                .register()
                .unwrap();
            assert_eq!(
                app.module::<ServerError, StaticVer01>(api2, toml)
                    .unwrap()
                    .register()
                    .unwrap_err(),
                DispatchError::ConflictingModules { prefix, conflict }.into()
            );
        }
    }

    #[async_std::test]
    async fn test_singleton_api() {
        setup_test();

        // If there is only one API, it should be possible to register it with an empty prefix.
        let toml = toml! {
            [route.test]
            PATH = ["/test"]
        };
        let mut app = App::<_, ServerError>::with_state(());
        let mut api = app.module::<ServerError, StaticVer01>("", toml).unwrap();
        api.with_version("0.1.0".parse().unwrap())
            .get("test", |_, _| async move { Ok("response") }.boxed())
            .unwrap();
        api.register().unwrap();

        let port = pick_unused_port().unwrap();
        spawn(app.serve(format!("0.0.0.0:{port}"), StaticVer01::instance()));
        let client = Client::new(format!("http://localhost:{port}").parse().unwrap()).await;

        // Test an endpoint.
        let res = client.get("/test").send().await.unwrap();
        assert_eq!(
            res.status(),
            StatusCode::OK,
            "{}",
            res.text().await.unwrap()
        );
        assert_eq!(res.json::<String>().await.unwrap(), "response");

        // Test healthcheck and version endpoints. Since these would ordinarily conflict with the
        // app-level healthcheck and version endpoints for an API with no prefix, we only get the
        // API-level endpoints, so that a singleton API behaves like a normal API, while app-level
        // stuff is reserved for non-trivial applications with more than one API.
        let res = client.get("/healthcheck").send().await.unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(
            res.json::<HealthStatus>().await.unwrap(),
            HealthStatus::Available
        );

        let res = client.get("/version").send().await.unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(
            res.json::<ApiVersion>().await.unwrap(),
            ApiVersion {
                api_version: Some("0.1.0".parse().unwrap()),
                spec_version: "0.1.0".parse().unwrap(),
            },
        );
    }

    #[async_std::test]
    async fn test_multi_segment() {
        setup_test();

        let toml = toml! {
            [route.test]
            PATH = ["/test"]
        };
        let mut app = App::<_, ServerError>::with_state(());

        for name in ["a", "b"] {
            let path = format!("api/{name}");
            let mut api = app
                .module::<ServerError, StaticVer01>(&path, toml.clone())
                .unwrap();
            api.with_version("0.1.0".parse().unwrap())
                .get("test", move |_, _| async move { Ok(name) }.boxed())
                .unwrap();
            api.register().unwrap();
        }

        let port = pick_unused_port().unwrap();
        spawn(app.serve(format!("0.0.0.0:{port}"), StaticVer01::instance()));
        let client = Client::new(format!("http://localhost:{port}").parse().unwrap()).await;

        for api in ["a", "b"] {
            tracing::info!(api, "testing api");

            // Test an endpoint.
            let res = client.get(&format!("api/{api}/test")).send().await.unwrap();
            assert_eq!(res.status(), StatusCode::OK);
            assert_eq!(res.json::<String>().await.unwrap(), api);

            // Test healthcheck.
            let res = client
                .get(&format!("api/{api}/healthcheck"))
                .send()
                .await
                .unwrap();
            assert_eq!(res.status(), StatusCode::OK);
            assert_eq!(
                res.json::<HealthStatus>().await.unwrap(),
                HealthStatus::Available
            );

            // Test version.
            let res = client
                .get(&format!("api/{api}/version"))
                .send()
                .await
                .unwrap();
            assert_eq!(res.status(), StatusCode::OK);
            assert_eq!(
                res.json::<ApiVersion>().await.unwrap().api_version.unwrap(),
                "0.1.0".parse().unwrap()
            );
        }

        // Test app-level healthcheck.
        let res = client.get("healthcheck").send().await.unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(
            res.json::<AppHealth>().await.unwrap(),
            AppHealth {
                status: HealthStatus::Available,
                modules: [
                    ("api/a".into(), [(0, StatusCode::OK)].into()),
                    ("api/b".into(), [(0, StatusCode::OK)].into()),
                ]
                .into()
            }
        );

        // Test app-level version.
        let res = client.get("version").send().await.unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(
            res.json::<AppVersion>().await.unwrap().modules,
            [
                (
                    "api/a".into(),
                    vec![ApiVersion {
                        api_version: Some("0.1.0".parse().unwrap()),
                        spec_version: "0.1.0".parse().unwrap(),
                    }]
                ),
                (
                    "api/b".into(),
                    vec![ApiVersion {
                        api_version: Some("0.1.0".parse().unwrap()),
                        spec_version: "0.1.0".parse().unwrap(),
                    }]
                ),
            ]
            .into()
        );
    }
}
