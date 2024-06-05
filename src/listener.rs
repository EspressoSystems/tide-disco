// Copyright (c) 2022 Espresso Systems (espressosys.com)
// This file is part of the tide-disco library.

// You should have received a copy of the MIT License
// along with the tide-disco library. If not, see <https://mit-license.org/>.

use crate::StatusCode;
use async_lock::Semaphore;
use async_std::{
    net::TcpListener,
    sync::Arc,
    task::{sleep, spawn},
};
use async_trait::async_trait;
use derivative::Derivative;
use futures::stream::StreamExt;
use std::{
    fmt::{self, Display, Formatter},
    io::{self, ErrorKind},
    net::SocketAddr,
    time::Duration,
};
use tide::{
    http,
    listener::{ListenInfo, Listener, ToListener},
    Server,
};

/// TCP listener which accepts only a limited number of connections at a time.
///
/// This listener is based on [`tide::listener::TcpListener`] and should match the semantics of that
/// listener in every way, accept that when there are more simultaneous outstanding requests than
/// the configured limit, excess requests will fail immediately with error code 429 (Too Many
/// Requests).
#[derive(Derivative)]
#[derivative(Debug(bound = "State: Send + Sync + 'static"))]
pub struct RateLimitListener<State> {
    addr: SocketAddr,
    listener: Option<TcpListener>,
    server: Option<Server<State>>,
    info: Option<ListenInfo>,
    permit: Arc<Semaphore>,
}

impl<State> RateLimitListener<State> {
    /// Listen at the given address.
    pub fn new(addr: SocketAddr, limit: usize) -> Self {
        Self {
            addr,
            listener: None,
            server: None,
            info: None,
            permit: Arc::new(Semaphore::new(limit)),
        }
    }

    /// Listen at the given port on all interfaces.
    pub fn with_port(port: u16, limit: usize) -> Self {
        Self::new(([0, 0, 0, 0], port).into(), limit)
    }
}

#[async_trait]
impl<State> Listener<State> for RateLimitListener<State>
where
    State: Clone + Send + Sync + 'static,
{
    async fn bind(&mut self, app: Server<State>) -> io::Result<()> {
        if self.server.is_some() {
            return Err(io::Error::new(
                ErrorKind::AlreadyExists,
                "`bind` should only be called once",
            ));
        }
        self.server = Some(app);
        self.listener = Some(TcpListener::bind(&[self.addr][..]).await?);

        // Format the listen information.
        let conn_string = format!("{}", self);
        let transport = "tcp".to_owned();
        let tls = false;
        self.info = Some(ListenInfo::new(conn_string, transport, tls));

        Ok(())
    }

    async fn accept(&mut self) -> io::Result<()> {
        let server = self.server.take().ok_or_else(|| {
            io::Error::other("`Listener::bind` must be called before `Listener::accept`")
        })?;
        let listener = self.listener.take().ok_or_else(|| {
            io::Error::other("`Listener::bind` must be called before `Listener::accept`")
        })?;

        let mut incoming = listener.incoming();
        while let Some(stream) = incoming.next().await {
            match stream {
                Err(err) if is_transient_error(&err) => continue,
                Err(err) => {
                    tracing::warn!(%err, "TCP error");
                    sleep(Duration::from_millis(500)).await;
                    continue;
                }
                Ok(stream) => {
                    let app = server.clone();
                    let permit = self.permit.clone();
                    spawn(async move {
                        let local_addr = stream.local_addr().ok();
                        let peer_addr = stream.peer_addr().ok();

                        let fut = async_h1::accept(stream, |mut req| async {
                            // Handle the request if we can get a permit.
                            if let Some(_guard) = permit.try_acquire() {
                                req.set_local_addr(local_addr);
                                req.set_peer_addr(peer_addr);
                                app.respond(req).await
                            } else {
                                // Otherwise, we are rate limited. Respond immediately with an
                                // error.
                                Ok(http::Response::new(StatusCode::TOO_MANY_REQUESTS))
                            }
                        });

                        if let Err(error) = fut.await {
                            tracing::error!(%error, "HTTP error");
                        }
                    });
                }
            };
        }
        Ok(())
    }

    fn info(&self) -> Vec<ListenInfo> {
        match &self.info {
            Some(info) => vec![info.clone()],
            None => vec![],
        }
    }
}

impl<State> ToListener<State> for RateLimitListener<State>
where
    State: Clone + Send + Sync + 'static,
{
    type Listener = Self;

    fn to_listener(self) -> io::Result<Self::Listener> {
        Ok(self)
    }
}

impl<State> Display for RateLimitListener<State> {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        match &self.listener {
            Some(listener) => {
                let addr = listener.local_addr().expect("Could not get local addr");
                write!(f, "http://{}", addr)
            }
            None => write!(f, "http://{}", self.addr),
        }
    }
}

fn is_transient_error(e: &io::Error) -> bool {
    matches!(
        e.kind(),
        ErrorKind::ConnectionRefused | ErrorKind::ConnectionAborted | ErrorKind::ConnectionReset
    )
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::{
        error::ServerError,
        testing::{setup_test, Client},
        App,
    };
    use futures::future::{try_join_all, FutureExt};
    use portpicker::pick_unused_port;
    use toml::toml;
    use vbs::version::{StaticVersion, StaticVersionType};

    type StaticVer01 = StaticVersion<0, 1>;

    #[async_std::test]
    async fn test_rate_limiting() {
        setup_test();

        let mut app = App::<_, ServerError>::with_state(());
        let api_toml = toml! {
            [route.test]
            PATH = ["/test"]
            METHOD = "GET"
        };
        {
            let mut api = app
                .module::<ServerError, StaticVer01>("mod", api_toml)
                .unwrap();
            api.get("test", |_req, _state| {
                async move {
                    // Make a really slow endpoint so we can have many simultaneous requests.
                    sleep(Duration::from_secs(30)).await;
                    Ok(())
                }
                .boxed()
            })
            .unwrap();
        }

        let limit = 10;
        let port = pick_unused_port().unwrap();
        spawn(app.serve(
            RateLimitListener::with_port(port, limit),
            StaticVer01::instance(),
        ));
        let client = Client::new(format!("http://localhost:{port}").parse().unwrap()).await;

        // Start the maximum number of simultaneous requests.
        let reqs = (0..limit)
            .map(|_| spawn(client.get("mod/test").send()))
            .collect::<Vec<_>>();

        // Wait a bit for those requests to get accepted.
        sleep(Duration::from_secs(5)).await;

        // The next request gets rate limited.
        let res = client.get("mod/test").send().await.unwrap();
        assert_eq!(StatusCode::TOO_MANY_REQUESTS, res.status());

        // The other requests eventually complete successfully.
        for res in try_join_all(reqs).await.unwrap() {
            assert_eq!(StatusCode::OK, res.status());
        }
    }
}
