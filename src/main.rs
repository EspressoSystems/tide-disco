use crate::HealthStatus::*;
use routefinder::Router;
use std::env;
use std::path::PathBuf;
use std::string::ToString;
use std::sync::{Arc, RwLock};
use tide::prelude::*;
use tide::{
    http::{mime, StatusCode},
    Request, Response,
};
use tide_disco::load_messages;

#[derive(Clone, Debug, Deserialize, strum_macros::Display)]
pub enum HealthStatus {
    Starting,
    Available,
    Stopping,
}

#[derive(Clone, Debug, Deserialize)]
struct WebState {
    health_status: Arc<RwLock<HealthStatus>>,
}

#[derive(Clone, Debug, Deserialize)]
struct Animal {
    name: String,
    legs: u16,
}

async fn order_shoes(mut req: Request<WebState>) -> tide::Result {
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

async fn disco_web_handler(_req: Request<WebState>) -> tide::Result {
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
async fn healthcheck(req: tide::Request<WebState>) -> Result<tide::Response, tide::Error> {
    let status = req.state().health_status.read().unwrap()/*.await*/;
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

#[async_std::main]
async fn main() -> tide::Result<()> {
    exercise_router();

    let cwd = env::current_dir().unwrap();
    let api_path = [cwd, "api/api.toml".into()].iter().collect::<PathBuf>();
    let api = load_messages(&api_path);
    println!("{}", api["meta"]["FORMAT_VERSION"]);

    tide::log::start();
    let web_state = WebState {
        health_status: Arc::new(RwLock::new(Available)),
    };
    let mut web_server = tide::with_state(web_state.clone());
    web_server.with(tide::log::LogMiddleware::new());

    web_server.at("/orders/shoes").post(order_shoes);
    web_server.at("/help").get(disco_web_handler);
    web_server.at("/healthcheck").get(healthcheck);

    println!("{}", *web_server.state().health_status.read().unwrap());
    let base_url: &str = "127.0.0.1:8080";
    web_server.listen(base_url).await?;
    Ok(())
}

// TODO Show how to modify WebState
// TODO Add signal handler for orderly shutdown on ^C, etc.
// TODO Take base_url from an environment variable
// TODO Take api.toml path from an environment variable
// TODO Command line arguments
// TODO CSS
// TODO Web form
