// Copyright (c) 2022 Espresso Systems (espressosys.com)
// This file is part of the tide-disco library.

// You should have received a copy of the MIT License
// along with the tide-disco library. If not, see <https://mit-license.org/>.

//! Support for routes using the Prometheus metrics format.

use crate::{
    method::ReadState,
    request::RequestParams,
    route::{self, RouteError},
};
use async_trait::async_trait;
use derive_more::From;
use futures::future::{BoxFuture, FutureExt};
use prometheus::{Encoder, TextEncoder};
use std::{borrow::Cow, error::Error, fmt::Debug};

pub trait Metrics {
    type Error: Debug + Error;

    fn export(&self) -> Result<String, Self::Error>;
}

impl Metrics for prometheus::Registry {
    type Error = prometheus::Error;

    fn export(&self) -> Result<String, Self::Error> {
        let mut buffer = vec![];
        let encoder = TextEncoder::new();
        let metric_families = self.gather();
        encoder.encode(&metric_families, &mut buffer)?;
        String::from_utf8(buffer).map_err(|err| {
            prometheus::Error::Msg(format!(
                "could not convert Prometheus output to UTF-8: {err}",
            ))
        })
    }
}

/// A [Handler](route::Handler) which delegates to an async function returning metrics.
///
/// The function type `F` should be callable as
/// `async fn(RequestParams, &State) -> Result<&R, Error>`. The [Handler] implementation will
/// automatically convert the result `R` to a [tide::Response] by exporting the [Metrics] to text,
/// or the error `Error` to a [RouteError] using [RouteError::AppSpecific].
///
/// # Limitations
///
/// [Like many function parameters](crate#boxed-futures) in [tide_disco](crate), the handler
/// function is required to return a [BoxFuture].
#[derive(From)]
pub(crate) struct Handler<F>(F);

#[async_trait]
impl<F, T, State, Error> route::Handler<State, Error> for Handler<F>
where
    F: 'static + Send + Sync + Fn(RequestParams, &State::State) -> BoxFuture<Result<Cow<T>, Error>>,
    T: 'static + Clone + Metrics,
    State: 'static + Send + Sync + ReadState,
    Error: 'static,
{
    async fn handle(
        &self,
        req: RequestParams,
        state: &State,
    ) -> Result<tide::Response, RouteError<Error>> {
        let exported = state
            .read(|state| {
                let fut = (self.0)(req, state);
                async move {
                    let metrics = fut.await.map_err(RouteError::AppSpecific)?;
                    metrics
                        .export()
                        .map_err(|err| RouteError::ExportMetrics(err.to_string()))
                }
                .boxed()
            })
            .await?;
        Ok(exported.into())
    }
}
