use async_std::sync::{Arc, RwLock};
use async_std::task::spawn;
use async_std::task::JoinHandle;
use clap::CommandFactory;
use config::{Config, ConfigError};
use routefinder::Router;
use serde::Deserialize;
use std::fs::read_to_string;
use std::{
    collections::HashMap,
    env,
    path::{Path, PathBuf},
};
use tide::{
    http::headers::HeaderValue,
    http::mime,
    security::{CorsMiddleware, Origin},
    Request, Response, StatusCode,
};
use toml::value::Value;
use tracing::info;
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

/// Load the web API or panic
pub fn load_api(path: &Path) -> toml::Value {
    let messages = read_to_string(&path).unwrap_or_else(|_| panic!("Unable to read {:?}.", &path));
    let api: toml::Value =
        toml::from_str(&messages).unwrap_or_else(|_| panic!("Unable to parse {:?}.", &path));
    if let Err(err) = check_api(api.clone()) {
        panic!("{}", err);
    }
    api
}

/// Return a JSON expression with status 200 indicating the server
/// is up and running. The JSON expression is normally
///    {"status": "Available"}
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

// Get a string from a toml::Value or panic.
fn vs(v: &Value) -> &str {
    v.as_str().expect(&format!(
        "Expecting TOML string, but found type {}: {:?}",
        v.type_str(),
        v
    ))
}

// Get a string from an array toml::Value or panic.
fn vk(v: &Value, key: &str) -> String {
    v[key]
        .as_str()
        .expect(&format!(
            "Expecting TOML string for {}, but found type {}",
            key,
            v[key].type_str()
        ))
        .to_string()
}

/// Compose `api.toml` into HTML.
///
/// This function iterates over the routes, adding headers and HTML
/// class attributes to make a documentation page for the web API.
///
/// The results of this could be precomputed and cached.
pub async fn compose_help(
    req: tide::Request<AppServerState>,
) -> Result<tide::Response, tide::Error> {
    let api = &req.state().app_state;
    let meta = &api["meta"];
    let version = vk(meta, "FORMAT_VERSION");
    let mut help = vk(meta, "HTML_TOP")
        .to_owned()
        .replace("{{FORMAT_VERSION}}", &version);
    if let Some(api_map) = api["route"].as_table() {
        api_map.values().for_each(|entry| {
            let paths = entry["PATH"].as_array().expect("Expecting TOML array.");
            let first_path = vs(&paths[0]);
            let first_segment = first_path.split_once('/').unwrap_or((first_path, "")).0;
            help += &vk(meta, "HEADING_ENTRY")
                .to_owned()
                .replace("{{NAME}}", &first_segment);
            help += &vk(meta, "HEADING_ROUTES");
            for path in paths.iter() {
                help += &vk(meta, "ROUTE_PATH")
                    .to_owned()
                    .replace("{{PATH}}", vs(&path));
            }
            help += &vk(meta, "HEADING_PARAMETERS");
            help += &vk(meta, "PARAMETER_TABLE_OPEN");
            let mut has_parameters = false;
            for (parameter, ptype) in entry
                .as_table()
                .expect("Route definitions must be tables in api.toml")
                .iter()
            {
                if parameter.starts_with(':') {
                    has_parameters = true;
                    let pname = parameter.strip_prefix(':').unwrap();
                    help += &vk(meta, "PARAMETER_ROW")
                        .to_owned()
                        .replace("{{NAME}}", pname)
                        .replace("{{TYPE}}", vs(&ptype));
                }
            }
            if !has_parameters {
                help += &vk(meta, "PARAMETER_NONE");
            }
            help += &vk(meta, "PARAMETER_TABLE_CLOSE");
            help += &vk(meta, "HEADING_DESCRIPTION");
            help += &markdown::to_html(vk(entry, "DOC").trim())
        });
    }
    help += &format!("{}\n", &vk(meta, "HTML_BOTTOM"));
    Ok(tide::Response::builder(200)
        .content_type(tide::http::mime::HTML)
        .body(help)
        .build())
}

pub async fn disco_web_handler(req: Request<AppServerState>) -> tide::Result {
    info!("url: {}", req.url());
    Ok(Response::builder(StatusCode::Ok)
        .body(vk(&req.state().app_state["meta"], "MINIMAL_HTML"))
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

    web_server.at("/help").get(compose_help);
    web_server.at("/healthcheck").get(healthcheck);
    web_server.at("/").all(disco_web_handler);
    web_server.at("/*").all(disco_web_handler);
    web_server.at("/public").serve_dir("public/media/")?;

    Ok(spawn(web_server.listen(base_url.to_string())))
}

/// Get the path to `api.toml`
pub fn get_api_path(api_toml: &str) -> PathBuf {
    [env::current_dir().unwrap(), api_toml.into()]
        .iter()
        .collect::<PathBuf>()
}

/// Convert the command line arguments for the config-rs crate
fn get_cmd_line_map<Args: CommandFactory>() -> config::Environment {
    config::Environment::default().source(Some({
        let mut cla = HashMap::new();
        let matches = Args::command().get_matches();
        for arg in Args::command().get_arguments() {
            if let Some(value) = matches.value_of(arg.get_id()) {
                let key = arg.get_id().replace('-', "_");
                cla.insert(key, value.to_owned());
            }
        }
        cla
    }))
}

/// Get the application configuration
///
/// Gets the configuration from
/// - Defaults in the source
/// - A configuration file config/app.toml
/// - Command line arguments
/// - Environment variables
/// Last one wins. Additional file sources can be added.
pub fn get_settings<Args: CommandFactory>() -> Result<Config, ConfigError> {
    // In the config-rs crate, environment variable names are
    // converted to lower case, but keys in files are not, so if we
    // want environment variables to override file value, we must make
    // file keys lower case. This is a config-rs bug. See
    // https://github.com/mehcode/config-rs/issues/340
    Config::builder()
        .set_default("base_url", "http://localhost/default")?
        .set_default("api_toml", "api/api.toml")?
        .add_source(config::File::with_name("config/app.toml"))
        .add_source(get_cmd_line_map::<Args>())
        .add_source(config::Environment::with_prefix("APP"))
        .build()
}

pub fn exercise_router() {
    let mut router = Router::new();
    router.add("/*", 1).unwrap();
    router.add("/hello", 2).unwrap();
    router.add("/:greeting", 3).unwrap();
    router.add("/hey/:world", 4).unwrap();
    router.add("/hey/earth", 5).unwrap();
    router.add("/:greeting/:world/*", 6).unwrap();

    assert_eq!(*router.best_match("/hey/earth").unwrap(), 5);
    assert_eq!(
        router
            .best_match("/hey/mars")
            .unwrap()
            .captures()
            .get("world"),
        Some("mars")
    );
    assert_eq!(router.matches("/hello").len(), 3);
    assert_eq!(router.matches("/").len(), 1);
}
