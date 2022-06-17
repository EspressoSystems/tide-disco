use serde::{Deserialize, Serialize};
use snafu::Snafu;
use std::fs;
use std::io;
use tide_disco::{http::StatusCode, Api, App};

#[derive(Clone, Debug, Deserialize, Serialize, Snafu)]
enum HelloError {
    Goodbye {
        status: StatusCode,
        farewell: String,
    },
}

impl tide_disco::Error for HelloError {
    fn catch_all(status: StatusCode, farewell: String) -> Self {
        Self::Goodbye { status, farewell }
    }

    fn status(&self) -> StatusCode {
        match self {
            Self::Goodbye { status, .. } => *status,
        }
    }
}

#[async_std::main]
async fn main() -> io::Result<()> {
    // Configure logs with timestamps and settings from the RUST_LOG
    // environment variable.
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .try_init()
        .unwrap();

    let greeting = "Hello, world!".to_string();
    let mut app = App::<String, HelloError>::with_state(greeting);
    let mut api = Api::<String, HelloError>::new(toml::from_slice(&fs::read(
        "examples/hello-world/api.toml",
    )?)?)
    .unwrap();
    api.at("greeting", |req| async move { Ok(req.state().clone()) })
        .unwrap();
    app.register_module("", api).unwrap();
    app.serve("0.0.0.0:8080").await
}
