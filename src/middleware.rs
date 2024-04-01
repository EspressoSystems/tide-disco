use crate::{
    http::{self, content::Accept},
    mime,
    request::best_response_type,
    route::{self, Handler, RouteError},
    App, RequestParam, RequestParams,
};
use async_std::sync::Arc;
use futures::future::{BoxFuture, FutureExt};
use vbs::version::StaticVersionType;

/// A function to add error information to a response body.
///
/// This trait is object safe, so it can be used to dynamically dispatch to different strategies for
/// serializing the error depending on the format version being used.
pub(crate) trait ErrorHandler<Error>:
    Fn(&Accept, &Error, &mut tide::Response) + Send + Sync
{
}
impl<Error, F> ErrorHandler<Error> for F where
    F: Fn(&Accept, &Error, &mut tide::Response) + Send + Sync
{
}

/// Type-erase a format-specific error handler.
pub(crate) fn error_handler<Error, VER>() -> Arc<dyn ErrorHandler<Error>>
where
    Error: crate::Error,
    VER: StaticVersionType,
{
    Arc::new(|accept, error, res| {
        // Try to add the error to the response body using a format accepted by the client. If we
        // cannot do that (for example, if the client requested a format that is incompatible with a
        // serialized error) just add the error as a string using plaintext.
        let (body, content_type) = route::response_body::<_, Error, VER>(accept, &error)
            .unwrap_or_else(|_| (error.to_string().into(), mime::PLAIN));
        res.set_body(body);
        res.set_content_type(content_type);
    })
}

/// Server middleware which automatically populates the body of error responses.
///
/// If the response contains an error, the error is encoded into the [Error](crate::Error) type
/// (either by downcasting if the server has generated an instance of [Error](crate::Error), or by
/// converting to a [String] using [Display] if the error can not be downcasted to
/// [Error](crate::Error)). The resulting [Error](crate::Error) is then serialized and used as the
/// body of the response.
///
/// If the response does not contain an error, it is passed through unchanged.
pub(crate) struct AddErrorBody<E> {
    handler: Arc<dyn ErrorHandler<E>>,
}

impl<E> AddErrorBody<E> {
    pub(crate) fn new(handler: Arc<dyn ErrorHandler<E>>) -> Self {
        Self { handler }
    }

    pub(crate) fn with_version<VER>() -> Self
    where
        E: crate::Error,
        VER: StaticVersionType,
    {
        Self::new(error_handler::<E, VER>())
    }
}

impl<T, E> tide::Middleware<T> for AddErrorBody<E>
where
    T: Clone + Send + Sync + 'static,
    E: crate::Error,
{
    fn handle<'a, 'b, 't>(
        &'a self,
        req: tide::Request<T>,
        next: tide::Next<'b, T>,
    ) -> BoxFuture<'t, tide::Result>
    where
        'a: 't,
        'b: 't,
        Self: 't,
    {
        async {
            let accept = RequestParams::accept_from_headers(&req)?;
            let mut res = next.run(req).await;
            if let Some(error) = res.take_error() {
                let error = E::from_server_error(error);
                tracing::info!("responding with error: {}", error);
                (self.handler)(&accept, &error, &mut res);
                Ok(res)
            } else {
                Ok(res)
            }
        }
        .boxed()
    }
}

pub(crate) struct MetricsMiddleware {
    route: String,
    api: String,
    api_version: u64,
}

impl MetricsMiddleware {
    pub(crate) fn new(route: String, api: String, api_version: u64) -> Self {
        Self {
            route,
            api,
            api_version,
        }
    }
}

impl<State, Error, VER> tide::Middleware<Arc<App<State, Error, VER>>> for MetricsMiddleware
where
    State: Send + Sync + 'static,
    Error: crate::Error + Send + Sync + 'static,
    VER: StaticVersionType + Send + Sync + 'static,
{
    fn handle<'a, 'b, 't>(
        &'a self,
        req: tide::Request<Arc<App<State, Error, VER>>>,
        next: tide::Next<'b, Arc<App<State, Error, VER>>>,
    ) -> BoxFuture<'t, tide::Result>
    where
        'a: 't,
        'b: 't,
        Self: 't,
    {
        let route = self.route.clone();
        let api = self.api.clone();
        let version = self.api_version;
        async move {
            if req.method() != http::Method::Get {
                // Metrics only apply to GET requests. For other requests, proceed with normal
                // dispatching.
                return Ok(next.run(req).await);
            }
            // Look at the `Accept` header. If the requested content type is plaintext, we consider
            // it a metrics request. Other endpoints have typed responses yielding either JSON or
            // binary.
            let accept = RequestParams::accept_from_headers(&req)?;
            let reponse_ty =
                best_response_type(&accept, &[mime::PLAIN, mime::JSON, mime::BYTE_STREAM])?;
            if reponse_ty != mime::PLAIN {
                return Ok(next.run(req).await);
            }
            // This is a metrics request, abort the rest of the dispatching chain and run the
            // metrics handler.
            let route = &req.state().clone().apis[&api][&version][&route];
            let state = &*req.state().clone().state;
            let req = request_params(req, route.params()).await?;
            route
                .handle(req, state)
                .await
                .map_err(|err| match err {
                    RouteError::AppSpecific(err) => err,
                    _ => Error::from_route_error(err),
                })
                .map_err(|err| err.into_tide_error())
        }
        .boxed()
    }
}

pub(crate) async fn request_params<State, Error: crate::Error, VER: StaticVersionType>(
    req: tide::Request<Arc<App<State, Error, VER>>>,
    params: &[RequestParam],
) -> Result<RequestParams, tide::Error> {
    RequestParams::new(req, params)
        .await
        .map_err(|err| Error::from_request_error(err).into_tide_error())
}
