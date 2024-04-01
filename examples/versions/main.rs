// Copyright (c) 2022 Espresso Systems (espressosys.com)
// This file is part of the tide-disco library.

// You should have received a copy of the MIT License
// along with the tide-disco library. If not, see <https://mit-license.org/>.

use futures::FutureExt;
use std::io;
use tide_disco::{error::ServerError, Api, App};
use vbs::version::{StaticVersion, StaticVersionType};

type StaticVer01 = StaticVersion<0, 1>;

async fn serve(port: u16) -> io::Result<()> {
    let mut app = App::<_, ServerError>::with_state(());
    app.with_version(env!("CARGO_PKG_VERSION").parse().unwrap());

    let mut v1 =
        Api::<(), ServerError, StaticVer01>::from_file("examples/versions/v1.toml").unwrap();
    v1.with_version("1.0.0".parse().unwrap())
        .get("deleted", |_, _| async move { Ok("deleted") }.boxed())
        .unwrap();

    let mut v2 =
        Api::<(), ServerError, StaticVer01>::from_file("examples/versions/v2.toml").unwrap();
    v2.with_version("2.0.0".parse().unwrap())
        .get("added", |_, _| async move { Ok("added") }.boxed())
        .unwrap();

    app.register_module("api", v1)
        .unwrap()
        .register_module("api", v2)
        .unwrap();
    app.serve(format!("0.0.0.0:{}", port), StaticVer01::instance())
        .await
}

#[async_std::main]
async fn main() -> io::Result<()> {
    // Configure logs with timestamps and settings from the RUST_LOG environment variable.
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
        testing::{setup_test, Client},
        StatusCode, Url,
    };

    #[async_std::test]
    async fn smoketest() {
        // There are thorough tests for versioning and all the edge cases of version-aware routing
        // in app.rs. This test is just a basic smoketest to prevent us from shipping a broken
        // example.
        setup_test();

        let port = pick_unused_port().unwrap();
        spawn(serve(port));
        let url = Url::parse(&format!("http://localhost:{}/", port)).unwrap();
        let client = Client::new(url).await;

        assert_eq!(
            "deleted",
            client
                .get("v1/api/deleted")
                .send()
                .await
                .unwrap()
                .json::<String>()
                .await
                .unwrap()
        );
        assert_eq!(
            StatusCode::NotFound,
            client.get("v1/api/added").send().await.unwrap().status()
        );

        assert_eq!(
            "added",
            client
                .get("v2/api/added")
                .send()
                .await
                .unwrap()
                .json::<String>()
                .await
                .unwrap()
        );
        assert_eq!(
            StatusCode::NotFound,
            client.get("v2/api/deleted").send().await.unwrap().status()
        );

        assert_eq!(
            "added",
            client
                .get("api/added")
                .send()
                .await
                .unwrap()
                .json::<String>()
                .await
                .unwrap()
        );
        assert_eq!(
            StatusCode::NotFound,
            client.get("api/deleted").send().await.unwrap().status()
        );
    }
}
