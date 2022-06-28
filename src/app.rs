use crate::{
    api::{Api, ApiVersion},
    healthcheck::{HealthCheck, HealthStatus},
    request::{RequestParam, RequestParams},
    route::{self, health_check_response, respond_with, Handler, RouteError},
};
use async_std::sync::Arc;
use futures::future::BoxFuture;
use semver::Version;
use serde::{Deserialize, Serialize};
use serde_with::{serde_as, DisplayFromStr};
use snafu::Snafu;
use std::collections::hash_map::{Entry, HashMap};
use std::convert::Infallible;
use std::io;
use tide::{
    http::headers::HeaderValue,
    http::{content::Accept, mime},
    security::{CorsMiddleware, Origin},
    StatusCode,
};

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
                e.insert(api.map_err(Error::from));
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

impl<State: Send + Sync + 'static, Error: 'static + crate::Error> App<State, Error> {
    /// Serve the [App] asynchronously.
    pub async fn serve<L: ToListener<Arc<Self>>>(self, listener: L) -> io::Result<()> {
        let state = Arc::new(self);
        let mut server = tide::Server::with_state(state.clone());
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
            for route in api {
                for pattern in route.patterns() {
                    let name = route.name();
                    let prefix = prefix.clone();
                    server.at(&prefix).at(pattern).method(
                        route.method(),
                        move |req: tide::Request<Arc<Self>>| {
                            let name = name.clone();
                            let prefix = prefix.clone();
                            async move {
                                let route = &req.state().clone().apis[&prefix][&name];
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
                        },
                    );
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
                            respond_with(&mut Accept::from_headers(&req)?, api.version()).map_err(
                                |err| Error::from_route_error::<Infallible>(err).into_tide_error(),
                            )
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
                let mut accept = Accept::from_headers(req.headers())?;
                let res = state.health(req, app_state).await;
                Ok(health_check_response(&mut accept, res))
            });
        server
            .at("version")
            .get(|req: tide::Request<Arc<Self>>| async move {
                respond_with(&mut Accept::from_headers(&req)?, req.state().version())
                    .map_err(|err| Error::from_route_error::<Infallible>(err).into_tide_error())
            });

        server.listen(listener).await
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
fn add_error_body<'a, T: Clone + Send + Sync + 'static, E: crate::Error>(
    req: tide::Request<T>,
    next: tide::Next<'a, T>,
) -> BoxFuture<'a, tide::Result> {
    Box::pin(async {
        let mut accept = Accept::from_headers(&req)?;
        let mut res = next.run(req).await;
        if let Some(error) = res.take_error() {
            let error = E::from_server_error(error);
            tracing::warn!("responding with error: {}", error);
            // Try to add the error to the response body using a format accepted by the client. If
            // we cannot do that (for example, if the client requested a format that is incompatible
            // with a serialized error) just add the error as a string using plaintext.
            let (body, content_type) = route::response_body::<_, E>(&mut accept, &error)
                .unwrap_or_else(|_| (error.to_string().into(), mime::PLAIN));
            res.set_body(body);
            res.set_content_type(content_type);
            Ok(res)
        } else {
            Ok(res)
        }
    })
}
