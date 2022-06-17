use async_std::sync::RwLock;
use futures::FutureExt;
use serde::{Deserialize, Serialize};
use snafu::Snafu;
use std::fs;
use std::io;
use tide_disco::{http::StatusCode, Api, App};
use tracing::info;

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

    let mut app = App::<_, HelloError>::with_state(RwLock::new("Hello, world!".to_string()));
    let mut api = Api::<RwLock<String>, HelloError>::new(toml::from_slice(&fs::read(
        "examples/hello-world/api.toml",
    )?)?)
    .unwrap();
    api.get("greeting", |req, greeting| {
        async move {
            let name = req.string_param("name").unwrap();
            info!("called /greeting with :name = {}", name);
            Ok(format!("{}, {}", greeting, name,))
        }
        .boxed()
    })
    .unwrap();
    api.post("setgreeting", |req, greeting| {
        async move {
            let new_greeting = req.string_param("greeting").unwrap();
            info!("called /setgreeting with :greeting = {}", new_greeting);
            *greeting = new_greeting;
            Ok(())
        }
        .boxed()
    })
    .unwrap();
    app.register_module("", api).unwrap();
    app.serve("0.0.0.0:8080").await
}
