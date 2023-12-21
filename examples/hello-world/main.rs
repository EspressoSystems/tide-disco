// Copyright (c) 2022 Espresso Systems (espressosys.com)
// This file is part of the tide-disco library.

// You should have received a copy of the MIT License
// along with the tide-disco library. If not, see <https://mit-license.org/>.

use async_std::sync::RwLock;
use futures::FutureExt;
use serde::{Deserialize, Serialize};
use snafu::Snafu;
use std::io;
use tide_disco::{Api, App, Error, RequestError, StatusCode};
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

impl From<RequestError> for HelloError {
    fn from(err: RequestError) -> Self {
        Self::catch_all(StatusCode::BadRequest, err.to_string())
    }
}

async fn serve(port: u16) -> io::Result<()> {
    let mut app = App::<_, HelloError>::with_state(RwLock::new("Hello".to_string()));
    app.with_version(env!("CARGO_PKG_VERSION").parse().unwrap());

    let mut api =
        Api::<RwLock<String>, HelloError>::from_file("examples/hello-world/api.toml").unwrap();
    api.with_version(env!("CARGO_PKG_VERSION").parse().unwrap());

    // Can invoke by browsing
    //    `http://0.0.0.0:8080/hello/greeting/dude`
    // Note: "greeting" is the route name in `api.toml`. `[route.greeting]` is
    // unrelated to the route PATH list.
    api.get("greeting", |req, greeting| {
        async move {
            let name = req.string_param("name")?;
            info!("called /greeting with :name = {}", name);
            Ok(format!("{}, {}", greeting, name,))
        }
        .boxed()
    })
    .unwrap();

    // Can invoke with
    //    `curl -i -X POST http://0.0.0.0:8080/hello/greeting/yo`
    api.post("setgreeting", |req, greeting| {
        async move {
            let new_greeting = req.string_param("greeting")?;
            info!("called /setgreeting with :greeting = {}", new_greeting);
            *greeting = new_greeting.to_string();
            Ok(())
        }
        .boxed()
    })
    .unwrap();

    app.register_module("hello", api).unwrap();
    app.serve(format!("0.0.0.0:{}", port)).await
}

#[async_std::main]
async fn main() -> io::Result<()> {
    // Configure logs with timestamps and settings from the RUST_LOG
    // environment variable.
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .try_init()
        .unwrap();
    serve(8080).await
}

#[cfg(test)]
mod test {
    use super::*;
    use async_std::task::spawn;
    use portpicker::pick_unused_port;
    use surf::Url;
    use tide_disco::{
        api::ApiVersion,
        app::{AppHealth, AppVersion},
        healthcheck::HealthStatus,
        wait_for_server, SERVER_STARTUP_RETRIES, SERVER_STARTUP_SLEEP_MS,
    };
    use tracing_test::traced_test;

    #[async_std::test]
    #[traced_test]
    async fn test_get_set_greeting() {
        let port = pick_unused_port().unwrap();
        spawn(serve(port));
        let url = Url::parse(&format!("http://localhost:{}/hello/", port)).unwrap();
        wait_for_server(&url, SERVER_STARTUP_RETRIES, SERVER_STARTUP_SLEEP_MS).await;

        let mut res = surf::get(url.join("greeting/tester").unwrap())
            .send()
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::Ok);
        assert_eq!(res.body_json::<String>().await.unwrap(), "Hello, tester");

        let res = surf::post(url.join("greeting/Sup").unwrap())
            .send()
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::Ok);

        let mut res = surf::get(url.join("greeting/tester").unwrap())
            .send()
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::Ok);
        assert_eq!(res.body_json::<String>().await.unwrap(), "Sup, tester");
    }

    #[async_std::test]
    #[traced_test]
    async fn test_version() {
        let port = pick_unused_port().unwrap();
        spawn(serve(port));
        let url = Url::parse(&format!("http://localhost:{}/", port)).unwrap();
        wait_for_server(&url, SERVER_STARTUP_RETRIES, SERVER_STARTUP_SLEEP_MS).await;

        // Check the API version.
        let mut res = surf::get(url.join("hello/version").unwrap())
            .send()
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::Ok);
        let api_version = ApiVersion {
            api_version: Some(env!("CARGO_PKG_VERSION").parse().unwrap()),
            spec_version: "0.1.0".parse().unwrap(),
        };
        assert_eq!(res.body_json::<ApiVersion>().await.unwrap(), api_version);

        // Check the overall version.
        let mut res = surf::get(url.join("version").unwrap())
            .send()
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::Ok);
        assert_eq!(
            res.body_json::<AppVersion>().await.unwrap(),
            AppVersion {
                app_version: Some(env!("CARGO_PKG_VERSION").parse().unwrap()),
                disco_version: env!("CARGO_PKG_VERSION").parse().unwrap(),
                modules: [("hello".to_string(), api_version)]
                    .iter()
                    .cloned()
                    .collect(),
            }
        )
    }

    #[async_std::test]
    #[traced_test]
    async fn test_healthcheck() {
        let port = pick_unused_port().unwrap();
        spawn(serve(port));
        let url = Url::parse(&format!("http://localhost:{}/", port)).unwrap();
        wait_for_server(&url, SERVER_STARTUP_RETRIES, SERVER_STARTUP_SLEEP_MS).await;

        // Check the API health.
        let mut res = surf::get(url.join("hello/healthcheck").unwrap())
            .send()
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::Ok);
        // The example API does not have a custom healthcheck, so we just get the default response.
        assert_eq!(
            res.body_json::<HealthStatus>().await.unwrap(),
            HealthStatus::Available
        );

        // Check the overall health.
        let mut res = surf::get(url.join("healthcheck").unwrap())
            .send()
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::Ok);
        assert_eq!(
            res.body_json::<AppHealth>().await.unwrap(),
            AppHealth {
                status: HealthStatus::Available,
                modules: [("hello".to_string(), StatusCode::Ok)]
                    .iter()
                    .cloned()
                    .collect(),
            }
        )
    }
}
