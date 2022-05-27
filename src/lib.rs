use async_std::sync::{Arc, RwLock};
use async_std::task::spawn;
use async_std::task::JoinHandle;
use serde::Deserialize;
use std::fs::read_to_string;
use std::path::Path;
use tide::{
    http::headers::HeaderValue,
    http::mime,
    log,
    log::info,
    security::{CorsMiddleware, Origin},
    Request, Response, StatusCode,
};
use toml::value::Value;
use url::Url;

#[derive(Clone, Debug, Deserialize, strum_macros::Display)]
pub enum HealthStatus {
    Starting,
    Available,
    Stopping,
}

#[derive(Clone)]
pub struct ServerState<AppState> {
    pub health_status: Arc<RwLock<HealthStatus>>,
    pub app_state: AppState,
}

// TODO This one should be defined by the application
pub type AppState = Value;

pub type AppServerState = ServerState<AppState>;

pub fn check_api(_api: toml::Value) -> Result<(), String> {
    Ok(())
}

/// Load the message catalog or panic
pub fn load_messages(path: &Path) -> toml::Value {
    let messages = read_to_string(&path).unwrap_or_else(|_| panic!("Unable to read {:?}.", &path));
    let api: toml::Value =
        toml::from_str(&messages).unwrap_or_else(|_| panic!("Unable to parse {:?}.", &path));
    if let Err(err) = check_api(api.clone()) {
        panic!("{}", err);
    }
    api
}

/// Return a JSON expression with status 200 indicating the server
/// is up and running. The JSON expression is simply,
///    {"status": "available"}
/// When the server is running but unable to process requests
/// normally, a response with status 503 and payload {"status":
/// "unavailable"} should be added.
pub async fn healthcheck(
    req: tide::Request<AppServerState>,
) -> Result<tide::Response, tide::Error> {
    let status = req.state().health_status.read().await;
    Ok(tide::Response::builder(StatusCode::Ok)
        .content_type(mime::JSON)
        .body(tide::prelude::json!({"status": status.to_string() }))
        .build())
}

//----------------
#[derive(Clone, Debug, Deserialize)]
pub struct Animal {
    pub name: String,
    pub legs: u16,
}

pub async fn order_shoes(mut req: Request<AppServerState>) -> tide::Result {
    let Animal { name, legs } = req.body_json().await?;
    Ok(format!("Hello, {}! I've put in an order for {} shoes", name, legs).into())
}
//----------------

pub async fn disco_web_handler(req: Request<AppServerState>) -> tide::Result {
    info!("uri: {}", req.url());
    Ok(Response::builder(StatusCode::Ok)
        .body(
            req.state().app_state["meta"]["MINIMAL_HTML"]
                .as_str()
                .expect("Expected [meta] MINIMAL_HTML to be a string."),
        )
        .content_type(mime::HTML)
        .build())
}

// TODO This belongs in lib.rs or web.rs.
// TODO The routes should come from api.toml.
pub async fn init_web_server(
    base_url: &str,
    state: AppServerState,
) -> std::io::Result<JoinHandle<std::io::Result<()>>> {
    let base_url = Url::parse(base_url).unwrap();
    let mut web_server = tide::with_state(state);
    web_server.with(
        CorsMiddleware::new()
            .allow_methods("GET, POST".parse::<HeaderValue>().unwrap())
            .allow_headers("*".parse::<HeaderValue>().unwrap())
            .allow_origin(Origin::from("*"))
            .allow_credentials(true),
    );
    web_server.with(log::LogMiddleware::new());

    web_server.at("/orders/shoes").post(order_shoes);
    web_server.at("/help").get(disco_web_handler);
    web_server.at("/healthcheck").get(healthcheck);
    web_server.at("/").get(disco_web_handler);
    web_server.at("/*").get(disco_web_handler);
    web_server.at("/").post(disco_web_handler);
    web_server.at("/*").post(disco_web_handler);

    Ok(spawn(web_server.listen(base_url.to_string())))
}
