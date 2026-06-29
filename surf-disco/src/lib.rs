// Copyright (c) 2022 Espresso Systems (espressosys.com)
// This file is part of the surf-disco library.

// You should have received a copy of the MIT License
// along with the surf-disco library. If not, see <https://mit-license.org/>.

//! Surf Disco: a client library for [Tide Disco](https://tide-disco.docs.espressosys.com/tide_disco/) applications.
//!
//! # Quick Start
//!
//! ```
//! # use surf_disco::{Client, error::ClientError};
//! # use vbs::version::StaticVersion;
//! # async fn ex() {
//! let url = "http://localhost:50000".parse().unwrap();
//! let client: Client<ClientError, StaticVersion<0,1>> = Client::new(url);
//! let res: String = client.get("/app/route").send().await.unwrap();
//! # }
//! ```
//!
use serde::de::DeserializeOwned;
use std::time::Duration;
use vbs::version::StaticVersionType;

pub mod client;
pub mod error;
pub mod reexports;
pub mod request;
pub mod socket;

pub use client::{Client, ContentType};
pub use error::Error;
pub use request::Request;
pub use socket::SocketRequest;
pub use tide_disco::{
    http::{self, Method},
    StatusCode, Url,
};

/// Build an HTTP `GET` request.
pub fn get<T: DeserializeOwned, E: Error, VER: StaticVersionType>(url: Url) -> Request<T, E, VER> {
    Client::new(url).get("/")
}

/// Build an HTTP `POST` request.
pub fn post<T: DeserializeOwned, E: Error, VER: StaticVersionType>(url: Url) -> Request<T, E, VER> {
    Client::new(url).post("/")
}

/// Connect to a server, retrying if the server is not running.
///
/// This function will make an HTTP `GET` request to the server's `/healthcheck` endpoint, to test
/// if the server is available. If this request succeeds, [connect] returns `true`. Otherwise, the
/// client will continue retrying `/healthcheck` requests until `timeout` has elapsed (or forever,
/// if `timeout` is `None`). If the timeout expires before a `/healthcheck` request succeeds,
/// [connect] will return `false`.
pub async fn connect<E: Error, VER: StaticVersionType>(
    url: Url,
    timeout: Option<Duration>,
) -> bool {
    Client::<E, VER>::new(url).connect(timeout).await
}
