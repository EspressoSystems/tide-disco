use async_std::sync::{Arc, RwLock};
use async_std::task::spawn;
use async_std::task::JoinHandle;
use routefinder::Router;
use signal::{Interrupt, InterruptHandle};
use signal_hook::consts::{SIGINT, SIGTERM, SIGUSR1};
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
mod signal;

type AppState = u8;

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

const MINIMAL_HTML: &str = "<!doctype html>
<html lang='en'>
  <head>
    <meta charset='utf-8'>
    <title>Help</title>
  </head>
  <body>
    <p>Help text goes here.</p>
  </body>
</html>";

async fn disco_web_handler(_req: Request<AppServerState>) -> tide::Result {
    Ok(Response::builder(StatusCode::Ok)
        .body(MINIMAL_HTML)
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

#[async_std::main]
async fn main() -> tide::Result<()> {
    tide::log::start();

    exercise_router();

    // Load a TOML file and display something from it.
    // TODO Take api.toml path from an environment variable
    let cwd = env::current_dir().unwrap();
    let api_path = [cwd, "api/api.toml".into()].iter().collect::<PathBuf>();
    let api = load_messages(&api_path);
    println!("API version: {}", api["meta"]["FORMAT_VERSION"]);

    let web_state = AppServerState {
        health_status: Arc::new(RwLock::new(Starting)),
        app_state: 0,
    };

    // Demonstrate that we can read and write the web server state.
    println!("{}", *web_state.health_status.read().await);
    *web_state.health_status.write().await = Available;

    // TODO Take base_url from an environment variable
    let base_url: &str = "127.0.0.1:8080";

    let interrupt_handler = InterruptHandle::new(&[SIGINT, SIGTERM, SIGUSR1]);

    init_web_server(base_url, web_state)
        .await
        .unwrap_or_else(|err| {
            panic!("Web server exited with an error: {}", err);
        })
        .await?;

    interrupt_handler.finalize().await;

    Ok(())
}

// TODO Add signal handler for orderly shutdown on ^C, etc.
// TODO Command line arguments
// TODO CSS
// TODO Web form
