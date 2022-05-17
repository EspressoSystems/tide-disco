use tide::prelude::*;
use tide::{
    http::{mime, StatusCode},
    Request, Response,
};

use routefinder::Router;
use std::env;
use std::path::PathBuf;
use tide_disco::load_messages;

#[derive(Debug, Deserialize)]
struct Animal {
    name: String,
    legs: u16,
}

async fn order_shoes(mut req: Request<()>) -> tide::Result {
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

async fn disco_web_handler(_req: Request<()>) -> tide::Result {
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
async fn healthcheck(_req: tide::Request<()>) -> Result<tide::Response, tide::Error> {
    Ok(tide::Response::builder(StatusCode::Ok)
        .content_type(mime::JSON)
        .body(tide::prelude::json!({"status": "available"}))
        .build())
}

#[async_std::main]
async fn main() -> tide::Result<()> {
    let cwd = env::current_dir().unwrap();
    let api_path = [cwd, "api/api.toml".into()].iter().collect::<PathBuf>();
    let api = load_messages(&api_path);
    println!("{}", api["meta"]["FORMAT_VERSION"]);

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

    tide::log::start();
    let mut web = tide::new();
    web.with(tide::log::LogMiddleware::new());
    web.at("/orders/shoes").post(order_shoes);
    web.at("/help").get(disco_web_handler);
    web.at("/heathcheck").get(healthcheck);
    web.listen("127.0.0.1:8080").await?;
    Ok(())
}
