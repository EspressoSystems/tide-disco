use crate::{
    api::Api,
    request::RequestParams,
    route::{Handler, RouteError},
};
use async_std::sync::Arc;
use snafu::Snafu;
use std::collections::hash_map::{Entry, HashMap};
use std::io;
use tide::http::StatusCode;

pub use tide::listener::{Listener, ToListener};

/// A tide-disco server application.
///
/// An [App] is a collection of API modules, plus a global `State`. Modules can be registered by
/// constructing an [Api] for each module and calling [App::register_module]. Once all of the
/// desired modules are registered, the app can be converted into an asynchronous server task using
/// [App::server].
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

impl<State: Clone + Send + Sync + 'static, Error: 'static> App<State, Error> {
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
}

impl<State: Clone + Send + Sync + 'static, Error: 'static + crate::Error> App<State, Error> {
    /// Serve the [App] asynchronously.
    pub async fn serve<L: ToListener<Arc<Self>>>(self, listener: L) -> io::Result<()> {
        let state = Arc::new(self);
        let mut server = tide::Server::with_state(state.clone());
        for (prefix, api) in &state.apis {
            for route in api {
                let name = route.name();
                let prefix = prefix.clone();
                server.at(&prefix).at(&name).method(
                    route.method(),
                    move |req: tide::Request<Arc<Self>>| {
                        let name = name.clone();
                        let prefix = prefix.clone();
                        async move {
                            let route = &req.state().apis[&prefix][&name];
                            let req =
                                RequestParams::new(&req, req.state().state.clone(), route.params())
                                    .map_err(|err| {
                                        Error::from_request_error(err).into_tide_error()
                                    })?;
                            route
                                .handle(req)
                                .await
                                .map_err(|err| match err {
                                    RouteError::AppSpecific(err) => err.into(),
                                    _ => Error::from_route_error(err),
                                })
                                .map_err(|err| err.into_tide_error())
                        }
                    },
                );
            }
        }
        server.listen(listener).await
    }
}
