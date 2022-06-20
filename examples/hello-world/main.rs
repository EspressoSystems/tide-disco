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

async fn serve(port: u16) -> io::Result<()> {
    let mut app = App::<_, HelloError>::with_state(RwLock::new("Hello".to_string()));
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
    use tracing_test::traced_test;

    #[async_std::test]
    #[traced_test]
    async fn test_get_set_greeting() {
        let port = pick_unused_port().unwrap();
        spawn(serve(port));
        let url = Url::parse(&format!("http://localhost:{}", port)).unwrap();

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
}
