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
    api::{Api, ApiError, ApiVersion},
    healthcheck::{HealthCheck, HealthStatus},
    http,
    method::Method,
    request::{RequestParam, RequestParams},
    route::{self, health_check_response, respond_with, Handler, Route, RouteError},
    socket::SocketError,
    Html,
};
use async_std::sync::Arc;
use futures::future::BoxFuture;
use include_dir::{include_dir, Dir};
use lazy_static::lazy_static;
use maud::html;
use semver::Version;
use serde::{Deserialize, Serialize};
use serde_with::{serde_as, DisplayFromStr};
use snafu::{ResultExt, Snafu};
use std::collections::hash_map::{Entry, HashMap};
use std::convert::Infallible;
use std::env;
use std::fs;
use std::io;
use std::ops::{Deref, DerefMut};
use std::path::PathBuf;
use tide::{
    http::{headers::HeaderValue, mime},
    security::{CorsMiddleware, Origin},
    StatusCode,
};
use tide_websockets::WebSocket;

pub use tide::listener::{Listener, ToListener};

/// A tide-disco server application.
///
/// An [App] is a collection of API modules, plus a global `State`. Modules can be registered by
/// constructing an [Api] for each module and calling [App::register_module]. Once all of the
/// desired modules are registered, the app can be converted into an asynchronous server task using
/// [App::serve].
pub struct App<State, Error> {
    // Map from base URL to module API.
    apis: HashMap<String, Api<State, Error>>,
    state: Arc<State>,
    app_version: Option<Version>,
}

/// An error encountered while building an [App].
#[derive(Clone, Debug, Snafu)]
pub enum AppError {
    Api { source: ApiError },
    ModuleAlreadyExists,
}

impl<State: Send + Sync + 'static, Error: 'static> App<State, Error> {
    /// Create a new [App] with a given state.
    pub fn with_state(state: State) -> Self {
        Self {
            apis: HashMap::new(),
            state: Arc::new(state),
            app_version: None,
        }
    }

    /// Create and register an API module.
    pub fn module<'a, ModuleError>(
        &'a mut self,
        base_url: &'a str,
        api: toml::Value,
    ) -> Result<Module<'a, State, Error, ModuleError>, AppError>
    where
        Error: From<ModuleError>,
        ModuleError: 'static + Send + Sync,
    {
        if self.apis.contains_key(base_url) {
            return Err(AppError::ModuleAlreadyExists);
        }

        Ok(Module {
            app: self,
            base_url,
            api: Some(Api::new(api).context(ApiSnafu)?),
        })
    }

    /// Register an API module.
    pub fn register_module<ModuleError>(
        &mut self,
        base_url: &str,
        api: Api<State, ModuleError>,
    ) -> Result<&mut Self, AppError>
    where
        Error: From<ModuleError>,
        ModuleError: 'static + Send + Sync,
    {
        match self.apis.entry(base_url.to_string()) {
            Entry::Occupied(_) => {
                return Err(AppError::ModuleAlreadyExists);
            }
            Entry::Vacant(e) => {
                let mut api = api.map_err(Error::from);
                api.set_name(base_url.to_string());
                e.insert(api);
            }
        }

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
    /// environment variable `CARGO_PKG_VERSION` and the [env] macro. As long as the following code
    /// is contained in the application crate, it should result in a reasonable version:
    ///
    /// ```
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
                .apis
                .iter()
                .map(|(name, api)| (name.clone(), api.version()))
                .collect(),
        }
    }

    /// Check the health of each registered module in response to a request.
    ///
    /// The response includes a status code for each module, which will be [StatusCode::Ok] if the
    /// module is healthy. Detailed health status from each module is not included in the response
    /// (due to type erasure) but can be queried using [module_health](Self::module_health) or by
    /// hitting the endpoint `GET /:module/healthcheck`.
    pub async fn health(&self, req: RequestParams, state: &State) -> AppHealth {
        let mut modules = HashMap::new();
        let mut status = HealthStatus::Available;
        for (name, api) in &self.apis {
            let health = api.health(req.clone(), state).await.status();
            if health != StatusCode::Ok {
                status = HealthStatus::Unhealthy;
            }
            modules.insert(name.clone(), health);
        }
        AppHealth { status, modules }
    }

    /// Check the health of the named module.
    ///
    /// The resulting [Response](tide::Response) has a status code which is [StatusCode::Ok] if the
    /// module is healthy. The response body is constructed from the results of the module's
    /// registered healthcheck handler. If the module does not have an explicit healthcheck
    /// handler, the response will be a [HealthStatus].
    ///
    /// If there is no module with the given name, returns [None].
    pub async fn module_health(
        &self,
        req: RequestParams,
        state: &State,
        module: &str,
    ) -> Option<tide::Response> {
        let api = self.apis.get(module)?;
        Some(api.health(req, state).await)
    }
}

static DEFAULT_PUBLIC_DIR: Dir<'_> = include_dir!("$CARGO_MANIFEST_DIR/public/media");
lazy_static! {
    static ref DEFAULT_PUBLIC_PATH: PathBuf = {
        // The contents of the default public directory are included in the binary. The first time
        // the default directory is used, if ever, we extract them to a directory on the host file
        // system and return the path to that directory.
        let path = dirs::data_local_dir()
            .unwrap_or_else(|| env::current_dir().unwrap_or_else(|_| PathBuf::from("./")))
            .join("tide-disco/public/media");
        // If the path already exists, move it aside so we can update it.
        let _ = fs::rename(&path, path.with_extension("old"));
        DEFAULT_PUBLIC_DIR.extract(&path).unwrap();
        path
    };
}

impl<State: Send + Sync + 'static, Error: 'static + crate::Error> App<State, Error> {
    /// Serve the [App] asynchronously.
    pub async fn serve<L: ToListener<Arc<Self>>>(self, listener: L) -> io::Result<()> {
        let state = Arc::new(self);
        let mut server = tide::Server::with_state(state.clone());
        for (name, api) in &state.apis {
            // Clippy complains if the only non-trivial operation in an `unwrap_or_else` closure is
            // a deref, but for `lazy_static` types, deref is an effectful operation that (in this
            // case) causes a directory to be renamed and another extracted. We only want to execute
            // this if we need to (if `api.public()` is `None`) so we disable the lint.
            #[allow(clippy::unnecessary_lazy_evaluations)]
            server
                .at("/public")
                .at(name)
                .serve_dir(api.public().unwrap_or_else(|| &DEFAULT_PUBLIC_PATH))?;
        }
        server.with(add_error_body::<_, Error>);
        server.with(
            CorsMiddleware::new()
                .allow_methods("GET, POST".parse::<HeaderValue>().unwrap())
                .allow_headers("*".parse::<HeaderValue>().unwrap())
                .allow_origin(Origin::from("*"))
                .allow_credentials(true),
        );

        for (prefix, api) in &state.apis {
            // Register routes for this API.
            let mut api_endpoint = server.at(prefix);
            for (path, routes) in api.routes_by_path() {
                let mut endpoint = api_endpoint.at(path);
                let routes = routes.collect::<Vec<_>>();
                if let Some(socket_route) =
                    routes.iter().find(|route| route.method() == Method::Socket)
                {
                    // If there is a socket route with this pattern, add the socket middleware to
                    // all endpoints registered under this pattern, so that any request with any
                    // method that has the socket upgrade headers will trigger a WebSockets upgrade.
                    Self::register_socket(prefix.to_owned(), &mut endpoint, socket_route);
                }
                // Register the HTTP routes.
                for route in routes {
                    if let Method::Http(method) = route.method() {
                        Self::register_route(prefix.to_owned(), &mut endpoint, route, method);
                    }
                }
            }

            // Register automatic routes for this API: `healthcheck` and `version`.
            {
                let prefix = prefix.clone();
                server
                    .at(&prefix)
                    .at("healthcheck")
                    .get(move |req: tide::Request<Arc<Self>>| {
                        let prefix = prefix.clone();
                        async move {
                            let api = &req.state().clone().apis[&prefix];
                            let state = req.state().clone();
                            Ok(api
                                .health(request_params(req, &[]).await?, &state.state)
                                .await)
                        }
                    });
            }
            {
                let prefix = prefix.clone();
                server
                    .at(&prefix)
                    .at("version")
                    .get(move |req: tide::Request<Arc<Self>>| {
                        let prefix = prefix.clone();
                        async move {
                            let api = &req.state().apis[&prefix];
                            let accept = RequestParams::accept_from_headers(&req)?;
                            respond_with(&accept, api.version()).map_err(|err| {
                                Error::from_route_error::<Infallible>(err).into_tide_error()
                            })
                        }
                    });
            }
        }

        // Register app-level automatic routes: `healthcheck` and `version`.
        server
            .at("healthcheck")
            .get(|req: tide::Request<Arc<Self>>| async move {
                let state = req.state().clone();
                let app_state = &*state.state;
                let req = request_params(req, &[]).await?;
                let accept = req.accept()?;
                let res = state.health(req, app_state).await;
                Ok(health_check_response(&accept, res))
            });
        server
            .at("version")
            .get(|req: tide::Request<Arc<Self>>| async move {
                let accept = RequestParams::accept_from_headers(&req)?;
                respond_with(&accept, req.state().version())
                    .map_err(|err| Error::from_route_error::<Infallible>(err).into_tide_error())
            });

        // Register catch-all routes for discoverability
        {
            server
                .at("/")
                .all(move |req: tide::Request<Arc<Self>>| async move {
                    Ok(html! {
                        "This is a Tide Disco app composed of the following modules:"
                        (req.state().list_apis())
                    })
                });
        }
        {
            server
                .at("/*")
                .all(move |req: tide::Request<Arc<Self>>| async move {
                    let api_name = req.url().path_segments().unwrap().next().unwrap();
                    let state = req.state();
                    if let Some(api) = state.apis.get(api_name) {
                        Ok(api.documentation())
                    } else {
                        Ok(html! {
                            "No valid route begins with \"" (api_name) "\". Try routes beginning
                            with one of the following API identifiers:"
                            (state.list_apis())
                        })
                    }
                });
        }

        server.listen(listener).await
    }

    fn list_apis(&self) -> Html {
        html! {
            ul {
                @for (name, api) in &self.apis {
                    li {
                        a href=(format!("/{}", name)) {(name)}
                        " "
                        (api.description())
                    }
                }
            }
        }
    }

    fn register_route(
        api: String,
        endpoint: &mut tide::Route<Arc<Self>>,
        route: &Route<State, Error>,
        method: http::Method,
    ) {
        let name = route.name();
        endpoint.method(method, move |req: tide::Request<Arc<Self>>| {
            let name = name.clone();
            let api = api.clone();
            async move {
                let route = &req.state().clone().apis[&api][&name];
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

    fn register_socket(
        api: String,
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
                        let route = &req.state().clone().apis[&api][&name];
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
        endpoint.all(move |req: tide::Request<Arc<Self>>| {
            let name = name.clone();
            let api = api.clone();
            async move {
                let route = &req.state().clone().apis[&api][&name];
                let state = &*req.state().clone().state;
                let req = request_params(req, route.params()).await?;
                route
                    .default_handler(req, state)
                    .map_err(|err| match err {
                        RouteError::AppSpecific(err) => err,
                        _ => Error::from_route_error(err),
                    })
                    .map_err(|err| err.into_tide_error())
            }
        });
    }
}

async fn request_params<State, Error: crate::Error>(
    req: tide::Request<Arc<App<State, Error>>>,
    params: &[RequestParam],
) -> Result<RequestParams, tide::Error> {
    RequestParams::new(req, params)
        .await
        .map_err(|err| Error::from_request_error(err).into_tide_error())
}

/// The health status of an application.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct AppHealth {
    /// The status of the overall application.
    ///
    /// [HealthStatus::Available] if all of the application's modules are healthy, otherwise a
    /// [HealthStatus] variant with [status](HealthCheck::status) other than 200.
    pub status: HealthStatus,
    /// The status of each registered module.
    pub modules: HashMap<String, StatusCode>,
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
    /// The version of each module registered with this application.
    pub modules: HashMap<String, ApiVersion>,

    /// The version of this application.
    #[serde_as(as = "Option<DisplayFromStr>")]
    pub app_version: Option<Version>,

    /// The version of the Tide Disco server framework.
    #[serde_as(as = "DisplayFromStr")]
    pub disco_version: Version,
}

/// Server middleware which automatically populates the body of error responses.
///
/// If the response contains an error, the error is encoded into the [Error](crate::Error) type
/// (either by downcasting if the server has generated an instance of [Error](crate::Error), or by
/// converting to a [String] using [Display] if the error can not be downcasted to
/// [Error](crate::Error)). The resulting [Error](crate::Error) is then serialized and used as the
/// body of the response.
///
/// If the response does not contain an error, it is passed through unchanged.
fn add_error_body<T: Clone + Send + Sync + 'static, E: crate::Error>(
    req: tide::Request<T>,
    next: tide::Next<T>,
) -> BoxFuture<tide::Result> {
    Box::pin(async {
        let accept = RequestParams::accept_from_headers(&req)?;
        let mut res = next.run(req).await;
        if let Some(error) = res.take_error() {
            let error = E::from_server_error(error);
            tracing::warn!("responding with error: {}", error);
            // Try to add the error to the response body using a format accepted by the client. If
            // we cannot do that (for example, if the client requested a format that is incompatible
            // with a serialized error) just add the error as a string using plaintext.
            let (body, content_type) = route::response_body::<_, E>(&accept, &error)
                .unwrap_or_else(|_| (error.to_string().into(), mime::PLAIN));
            res.set_body(body);
            res.set_content_type(content_type);
            Ok(res)
        } else {
            Ok(res)
        }
    })
}

pub struct Module<'a, State, Error, ModuleError>
where
    State: 'static + Send + Sync,
    Error: 'static + From<ModuleError>,
    ModuleError: 'static + Send + Sync,
{
    app: &'a mut App<State, Error>,
    base_url: &'a str,
    // This is only an [Option] so we can [take] out of it during [drop].
    api: Option<Api<State, ModuleError>>,
}

impl<'a, State, Error, ModuleError> Deref for Module<'a, State, Error, ModuleError>
where
    State: 'static + Send + Sync,
    Error: 'static + From<ModuleError>,
    ModuleError: 'static + Send + Sync,
{
    type Target = Api<State, ModuleError>;

    fn deref(&self) -> &Self::Target {
        self.api.as_ref().unwrap()
    }
}

impl<'a, State, Error, ModuleError> DerefMut for Module<'a, State, Error, ModuleError>
where
    State: 'static + Send + Sync,
    Error: 'static + From<ModuleError>,
    ModuleError: 'static + Send + Sync,
{
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.api.as_mut().unwrap()
    }
}

impl<'a, State, Error, ModuleError> Drop for Module<'a, State, Error, ModuleError>
where
    State: 'static + Send + Sync,
    Error: 'static + From<ModuleError>,
    ModuleError: 'static + Send + Sync,
{
    fn drop(&mut self) {
        self.app
            .register_module(self.base_url, self.api.take().unwrap())
            .unwrap();
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::{
        error::ServerError, socket::Connection, wait_for_server, Url, SERVER_STARTUP_RETRIES,
        SERVER_STARTUP_SLEEP_MS,
    };
    use async_std::{sync::RwLock, task::spawn};
    use async_tungstenite::{async_std::connect_async, tungstenite::Message};
    use futures::{FutureExt, SinkExt, StreamExt};
    use portpicker::pick_unused_port;
    use toml::toml;

    /// Test route dispatching for routes with the same path and different methods.
    #[async_std::test]
    async fn test_method_dispatch() {
        use crate::http::Method::*;

        let mut app = App::<_, ServerError>::with_state(RwLock::new(()));
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
        };
        {
            let mut api = app.module::<ServerError>("mod", api_toml).unwrap();
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
                |_req, mut conn: Connection<_, (), _>, _state| {
                    async move {
                        conn.send("SOCKET").await.unwrap();
                        Ok(())
                    }
                    .boxed()
                },
            )
            .unwrap();
        }
        let port = pick_unused_port().unwrap();
        let url: Url = format!("http://localhost:{}", port).parse().unwrap();
        spawn(app.serve(format!("0.0.0.0:{}", port)));
        wait_for_server(&url, SERVER_STARTUP_RETRIES, SERVER_STARTUP_SLEEP_MS).await;

        let client: surf::Client = surf::Config::new()
            .set_base_url(url.clone())
            .try_into()
            .unwrap();
        for method in [Get, Post, Put, Delete] {
            let mut res = client
                .request(method, url.join("mod/test").unwrap())
                .send()
                .await
                .unwrap();
            assert_eq!(res.status(), StatusCode::Ok);
            assert_eq!(res.body_json::<String>().await.unwrap(), method.to_string());
        }

        let mut socket_url = url.join("mod/test").unwrap();
        socket_url.set_scheme("ws").unwrap();
        let mut conn = connect_async(socket_url).await.unwrap().0;
        let msg = conn.next().await.unwrap().unwrap();
        let body: String = match msg {
            Message::Text(m) => serde_json::from_str(&m).unwrap(),
            Message::Binary(m) => bincode::deserialize(&m).unwrap(),
            m => panic!("expected Text or Binary message, but got {}", m),
        };
        assert_eq!(body, "SOCKET");
    }
}
