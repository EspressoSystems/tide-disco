use crate::{
    api::Api,
    healthcheck::{HealthCheck, HealthStatus},
    request::{RequestParam, RequestParams},
    route::{health_check_response, Handler, RouteError},
};
use async_std::sync::Arc;
use serde::{Deserialize, Serialize};
use snafu::Snafu;
use std::collections::hash_map::{Entry, HashMap};
use std::io;
use tide::{http::content::Accept, StatusCode};

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
                                let route = &req.state().apis[&prefix][&name];
                                let state = &*req.state().state;
                                let req = request_params(&req, route.params())?;
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

            // Register automatic routes for this API: `healthcheck`.
            let prefix = prefix.clone();
            server
                .at(&prefix)
                .at("healthcheck")
                .get(move |req: tide::Request<Arc<Self>>| {
                    let prefix = prefix.clone();
                    async move {
                        let api = &req.state().apis[&prefix];
                        Ok(api
                            .health(request_params(&req, &[])?, &*req.state().state)
                            .await)
                    }
                });
        }

        // Register app-level automatic routes: `healthcheck`.
        server
            .at("healthcheck")
            .get(|req: tide::Request<Arc<Self>>| async move {
                let state = req.state();
                let app_state = &*state.state;
                let req = request_params(&req, &[])?;
                let mut accept = Accept::from_headers(req.headers())?;
                let res = state.health(req, app_state).await;
                Ok(health_check_response(&mut accept, res))
            });

        server.listen(listener).await
    }
}

fn request_params<State, Error: crate::Error>(
    req: &tide::Request<Arc<App<State, Error>>>,
    params: &[RequestParam],
) -> Result<RequestParams, tide::Error> {
    RequestParams::new(req, params).map_err(|err| Error::from_request_error(err).into_tide_error())
}

/// The health status of an application.
#[derive(Clone, Debug, Deserialize, Serialize)]
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
