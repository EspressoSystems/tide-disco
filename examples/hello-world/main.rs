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
use vbs::version::StaticVersion;

type StaticVer01 = StaticVersion<0, 1>;

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
    let mut app = App::<_, HelloError, StaticVer01>::with_state(RwLock::new("Hello".to_string()));
    app.with_version(env!("CARGO_PKG_VERSION").parse().unwrap());

    let mut api =
        Api::<RwLock<String>, HelloError, StaticVer01>::from_file("examples/hello-world/api.toml")
            .unwrap();
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
    use tide_disco::{
        api::ApiVersion,
        app::{AppHealth, AppVersion},
        healthcheck::HealthStatus,
        testing::{setup_test, Client},
        Url,
    };

    #[async_std::test]
    async fn test_get_set_greeting() {
        setup_test();

        let port = pick_unused_port().unwrap();
        spawn(serve(port));
        let url = Url::parse(&format!("http://localhost:{}/hello/", port)).unwrap();
        let client = Client::new(url).await;

        let res = client.get("greeting/tester").send().await.unwrap();
        assert_eq!(res.status(), StatusCode::Ok);
        assert_eq!(res.json::<String>().await.unwrap(), "Hello, tester");

        let res = client.post("greeting/Sup").send().await.unwrap();
        assert_eq!(res.status(), StatusCode::Ok);

        let res = client.get("greeting/tester").send().await.unwrap();
        assert_eq!(res.status(), StatusCode::Ok);
        assert_eq!(res.json::<String>().await.unwrap(), "Sup, tester");
    }

    #[async_std::test]
    async fn test_version() {
        setup_test();

        let port = pick_unused_port().unwrap();
        spawn(serve(port));
        let url = Url::parse(&format!("http://localhost:{}/", port)).unwrap();
        let client = Client::new(url).await;

        // Check the API version.
        let res = client.get("hello/version").send().await.unwrap();
        assert_eq!(res.status(), StatusCode::Ok);
        let api_version = ApiVersion {
            api_version: Some(env!("CARGO_PKG_VERSION").parse().unwrap()),
            spec_version: "0.1.0".parse().unwrap(),
        };
        assert_eq!(res.json::<ApiVersion>().await.unwrap(), api_version);

        // Check the overall version.
        let res = client.get("version").send().await.unwrap();
        assert_eq!(res.status(), StatusCode::Ok);
        assert_eq!(
            res.json::<AppVersion>().await.unwrap(),
            AppVersion {
                app_version: Some(env!("CARGO_PKG_VERSION").parse().unwrap()),
                disco_version: env!("CARGO_PKG_VERSION").parse().unwrap(),
                modules: [("hello".to_string(), vec![api_version])].into()
            }
        )
    }

    #[async_std::test]
    async fn test_healthcheck() {
        setup_test();

        let port = pick_unused_port().unwrap();
        spawn(serve(port));
        let url = Url::parse(&format!("http://localhost:{}/", port)).unwrap();
        let client = Client::new(url).await;

        // Check the API health.
        let res = client.get("hello/healthcheck").send().await.unwrap();
        assert_eq!(res.status(), StatusCode::Ok);
        // The example API does not have a custom healthcheck, so we just get the default response.
        assert_eq!(
            res.json::<HealthStatus>().await.unwrap(),
            HealthStatus::Available
        );

        // Check the overall health.
        let res = client.get("healthcheck").send().await.unwrap();
        assert_eq!(res.status(), StatusCode::Ok);
        assert_eq!(
            res.json::<AppHealth>().await.unwrap(),
            AppHealth {
                status: HealthStatus::Available,
                modules: [("hello".to_string(), [(0, StatusCode::Ok)].into())].into(),
            }
        )
    }
}
