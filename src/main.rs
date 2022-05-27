use crate::signal::Interrupt;
use async_std::sync::{Arc, RwLock};
use async_std::task::spawn;
use async_std::task::JoinHandle;
use clap::{CommandFactory, Parser};
use config::{Config, ConfigError};
use routefinder::Router;
use signal::InterruptHandle;
use signal_hook::consts::{SIGINT, SIGTERM, SIGUSR1};
use std::collections::HashMap;
use std::env;
use std::path::PathBuf;
use std::process;
use tide::prelude::*;
use tide::{
    http::headers::HeaderValue,
    http::mime,
    security::{CorsMiddleware, Origin},
    Request, Response, StatusCode,
};
use tide_disco::HealthStatus::*;
use tide_disco::{load_messages, ServerState};
use toml::value::Value;
use url::Url;

mod signal;

#[derive(Parser, Debug)]
#[clap(author, version, about, long_about = None)]
struct Args {
    #[clap(long)]
    base_url: Option<Url>,
    #[clap(long)]
    api_toml: Option<PathBuf>,
}

type AppState = Value;

type AppServerState = ServerState<AppState>;

#[derive(Clone, Debug, Deserialize)]
struct Animal {
    name: String,
    legs: u16,
}

async fn order_shoes(mut req: Request<AppServerState>) -> tide::Result {
    let Animal { name, legs } = req.body_json().await?;
    Ok(format!("Hello, {}! I've put in an order for {} shoes", name, legs).into())
}

async fn disco_web_handler(req: Request<AppServerState>) -> tide::Result {
    Ok(Response::builder(StatusCode::Ok)
        .body(
            req.state().app_state["meta"]["MINIMAL_HTML"]
                .as_str()
                .expect("Expected [meta] MINIMAL_HTML to be a string."),
        )
        .content_type(mime::HTML)
        .build())
}

/// Return a JSON expression with status 200 indicating the server
/// is up and running. The JSON expression is simply,
///    {"status": "available"}
/// When the server is running but unable to process requests
/// normally, a response with status 503 and payload {"status":
/// "unavailable"} should be added.
async fn healthcheck(req: tide::Request<AppServerState>) -> Result<tide::Response, tide::Error> {
    let status = req.state().health_status.read().await;
    Ok(tide::Response::builder(StatusCode::Ok)
        .content_type(mime::JSON)
        .body(tide::prelude::json!({"status": status.to_string() }))
        .build())
}

fn exercise_router() {
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
    web_server.with(tide::log::LogMiddleware::new());

    web_server.at("/orders/shoes").post(order_shoes);
    web_server.at("/help").get(disco_web_handler);
    web_server.at("/healthcheck").get(healthcheck);

    Ok(spawn(web_server.listen(base_url.to_string())))
}

impl Interrupt for InterruptHandle {
    fn signal_action(signal: i32) {
        // TOOD modify web_state based on the signal.
        println!("\nReceived signal {}", signal);
        process::exit(1);
    }
}

///
fn get_api_path(api_toml: &str) -> PathBuf {
    [env::current_dir().unwrap(), api_toml.into()]
        .iter()
        .collect::<PathBuf>()
}

/// Convert the command line arguments for the config-rs crate
fn get_cmd_line_map() -> config::Environment {
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

/// Get the application configuration.
///
/// Gets the configuration from
/// - Defaults in the source
/// - A configuration file config/app.toml
/// - Command line arguments
/// - Environment variables
/// Last one wins. Additional file sources can be added.
fn get_settings() -> Result<Config, ConfigError> {
    // In the config-rs crate, environment variable names are
    // converted to lower case, but keys in files are not, so if we
    // want environment variables to override file value, we must make
    // file keys lower case. This is a config-rs bug. See
    // https://github.com/mehcode/config-rs/issues/340
    Config::builder()
        .set_default("base_url", "http://localhost/default")?
        .set_default("api_toml", "api/api.toml")?
        .add_source(config::File::with_name("config/app.toml"))
        .add_source(get_cmd_line_map())
        .add_source(config::Environment::with_prefix("APP"))
        .build()
}

#[async_std::main]
async fn main() -> tide::Result<()> {
    tide::log::start();

    let settings = get_settings()?;
    tide::log::info!("{:?}", settings);

    // Fetch the configuration values before any slow operations.
    let api_toml = &settings.get_string("api_toml")?;
    let base_url = &settings.get_string("base_url")?;

    exercise_router();

    // Load a TOML file and display something from it.
    let api = load_messages(&get_api_path(api_toml));
    tide::log::info!(
        "API version: {}",
        api["meta"]["FORMAT_VERSION"]
            .as_str()
            .expect("Expecting string.")
    );

    let web_state = AppServerState {
        health_status: Arc::new(RwLock::new(Starting)),
        app_state: api,
    };

    // Demonstrate that we can read and write the web server state.
    tide::log::info!("Health Status: {}", *web_state.health_status.read().await);
    *web_state.health_status.write().await = Available;

    let mut interrupt_handler = InterruptHandle::new(&[SIGINT, SIGTERM, SIGUSR1]);
    init_web_server(&base_url, web_state)
        .await
        .unwrap_or_else(|err| {
            panic!("Web server exited with an error: {}", err);
        })
        .await?;

    interrupt_handler.finalize().await;

    Ok(())
}

// TODO CSS
// TODO add favicon.ico
// TODO Web form
// TODO keys for set_default have no typo checking.
// TODO include timestamp in logs
// TODO make tide logs one line each
