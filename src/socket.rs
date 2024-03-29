// Copyright (c) 2022 Espresso Systems (espressosys.com)
// This file is part of the tide-disco library.

// You should have received a copy of the MIT License
// along with the tide-disco library. If not, see <https://mit-license.org/>.

//! An interface for asynchronous communication with clients, using WebSockets.

use crate::{
    http::{content::Accept, mime},
    method::Method,
    request::{best_response_type, RequestError, RequestParams},
    StatusCode,
};
use async_std::sync::Arc;
use futures::{
    future::BoxFuture,
    sink,
    stream::BoxStream,
    task::{Context, Poll},
    FutureExt, Sink, SinkExt, Stream, StreamExt, TryFutureExt,
};
use serde::{de::DeserializeOwned, Serialize};
use std::borrow::Cow;
use std::fmt::{self, Display, Formatter};
use std::marker::PhantomData;
use std::pin::Pin;
use tide_websockets::{
    tungstenite::protocol::frame::{coding::CloseCode, CloseFrame},
    Message, WebSocketConnection,
};
use vbs::{version::StaticVersionType, BinarySerializer, Serializer};

/// An error returned by a socket handler.
///
/// [SocketError] encapsulates application specific errors `E` returned by the user-installed
/// handler itself. It also includes errors in the socket protocol, such as failures to turn
/// messages sent by the user-installed handler into WebSockets messages.
#[derive(Debug)]
pub enum SocketError<E> {
    AppSpecific(E),
    Request(RequestError),
    Binary(anyhow::Error),
    Json(serde_json::Error),
    WebSockets(tide_websockets::Error),
    UnsupportedMessageType,
    Closed,
    IncorrectMethod { expected: Method, actual: Method },
}

impl<E> SocketError<E> {
    pub fn status(&self) -> StatusCode {
        match self {
            Self::Request(_) | Self::UnsupportedMessageType | Self::IncorrectMethod { .. } => {
                StatusCode::BadRequest
            }
            _ => StatusCode::InternalServerError,
        }
    }

    pub fn code(&self) -> CloseCode {
        CloseCode::Error
    }

    pub fn map_app_specific<E2>(self, f: &impl Fn(E) -> E2) -> SocketError<E2> {
        match self {
            Self::AppSpecific(e) => SocketError::AppSpecific(f(e)),
            Self::Request(e) => SocketError::Request(e),
            Self::Binary(e) => SocketError::Binary(e),
            Self::Json(e) => SocketError::Json(e),
            Self::WebSockets(e) => SocketError::WebSockets(e),
            Self::UnsupportedMessageType => SocketError::UnsupportedMessageType,
            Self::Closed => SocketError::Closed,
            Self::IncorrectMethod { expected, actual } => {
                SocketError::IncorrectMethod { expected, actual }
            }
        }
    }
}

impl<E: Display> Display for SocketError<E> {
    fn fmt(&self, f: &mut Formatter) -> fmt::Result {
        match self {
            Self::AppSpecific(e) => write!(f, "{}", e),
            Self::Request(e) => write!(f, "{}", e),
            Self::Binary(e) => write!(f, "error creating byte stream: {}", e),
            Self::Json(e) => write!(f, "error creating JSON message: {}", e),
            Self::WebSockets(e) => write!(f, "WebSockets protocol error: {}", e),
            Self::UnsupportedMessageType => {
                write!(f, "unsupported content type for WebSockets message")
            }
            Self::Closed => write!(f, "connection closed"),
            Self::IncorrectMethod { expected, actual } => write!(
                f,
                "endpoint must be called as {}, but was called as {}",
                expected, actual
            ),
        }
    }
}

impl<E> From<RequestError> for SocketError<E> {
    fn from(err: RequestError) -> Self {
        Self::Request(err)
    }
}

impl<E> From<anyhow::Error> for SocketError<E> {
    fn from(err: anyhow::Error) -> Self {
        Self::Binary(err)
    }
}

impl<E> From<serde_json::Error> for SocketError<E> {
    fn from(err: serde_json::Error) -> Self {
        Self::Json(err)
    }
}

impl<E> From<tide_websockets::Error> for SocketError<E> {
    fn from(err: tide_websockets::Error) -> Self {
        Self::WebSockets(err)
    }
}

#[derive(Clone, Copy, Debug)]
enum MessageType {
    Binary,
    Json,
}

/// A connection facilitating bi-directional, asynchronous communication with a client.
///
/// [Connection] implements [Stream], which can be used to receive `FromClient` messages from the
/// client, and [Sink] which can be used to send `ToClient` messages to the client.
pub struct Connection<ToClient: ?Sized, FromClient, Error, VER: StaticVersionType> {
    conn: WebSocketConnection,
    // [Sink] wrapper around `conn`
    sink: Pin<Box<dyn Send + Sink<Message, Error = SocketError<Error>>>>,
    accept: MessageType,
    #[allow(clippy::type_complexity)]
    _phantom: PhantomData<fn(&ToClient, &FromClient, &Error, &VER) -> ()>,
}

impl<ToClient: ?Sized, FromClient: DeserializeOwned, E, VER: StaticVersionType> Stream
    for Connection<ToClient, FromClient, E, VER>
{
    type Item = Result<FromClient, SocketError<E>>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        // Get a `Pin<&mut WebSocketConnection>` for the underlying connection, so we can use the
        // `Stream` implementation of that field.
        match self.pinned_inner().poll_next(cx) {
            Poll::Ready(None) => Poll::Ready(None),
            Poll::Ready(Some(Err(err))) => Poll::Ready(Some(Err(err.into()))),
            Poll::Ready(Some(Ok(msg))) => Poll::Ready(Some(match msg {
                Message::Binary(bytes) => {
                    Serializer::<VER>::deserialize(&bytes).map_err(SocketError::from)
                }
                Message::Text(s) => serde_json::from_str(&s).map_err(SocketError::from),
                _ => Err(SocketError::UnsupportedMessageType),
            })),
            Poll::Pending => Poll::Pending,
        }
    }
}

impl<ToClient: Serialize + ?Sized, FromClient, E, VER: StaticVersionType> Sink<&ToClient>
    for Connection<ToClient, FromClient, E, VER>
{
    type Error = SocketError<E>;

    fn poll_ready(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.sink.as_mut().poll_ready(cx).map_err(SocketError::from)
    }

    fn start_send(mut self: Pin<&mut Self>, item: &ToClient) -> Result<(), Self::Error> {
        let msg = match self.accept {
            MessageType::Binary => Message::Binary(Serializer::<VER>::serialize(item)?),
            MessageType::Json => Message::Text(serde_json::to_string(item)?),
        };
        self.sink
            .as_mut()
            .start_send(msg)
            .map_err(SocketError::from)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.sink.as_mut().poll_flush(cx).map_err(SocketError::from)
    }

    fn poll_close(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.sink.as_mut().poll_close(cx).map_err(SocketError::from)
    }
}

impl<ToClient: ?Sized, FromClient, Error, VER: StaticVersionType> Drop
    for Connection<ToClient, FromClient, Error, VER>
{
    fn drop(&mut self) {
        // This is the idiomatic way to implement [drop] for a type that uses pinning. Since [drop]
        // is implicitly called with `&mut self` even on types that were pinned, we place any
        // implementation inside [inner_drop], which takes `Pin<&mut Self>`, when the commpiler will
        // be able to check that we do not do anything that we couldn't have done on a
        // `Pin<&mut Self>`.
        //
        // The [drop] implementation for this type is trivial, and it would be safe to use the
        // automatically generated [drop] implementation, but we nonetheless implement [drop]
        // explicitly in the idiomatic fashion so that it is impossible to accidentally implement an
        // unsafe version of [drop] for this type in the future.

        // `new_unchecked` is okay because we know this value is never used again after being
        // dropped.
        inner_drop(unsafe { Pin::new_unchecked(self) });
        fn inner_drop<ToClient: ?Sized, FromClient, Error, VER: StaticVersionType>(
            _this: Pin<&mut Connection<ToClient, FromClient, Error, VER>>,
        ) {
            // Any logic goes here.
        }
    }
}

impl<ToClient: ?Sized, FromClient, E, VER: StaticVersionType>
    Connection<ToClient, FromClient, E, VER>
{
    fn new(accept: &Accept, conn: WebSocketConnection) -> Result<Self, SocketError<E>> {
        let ty = best_response_type(accept, &[mime::JSON, mime::BYTE_STREAM])?;
        let ty = if ty == mime::JSON {
            MessageType::Json
        } else if ty == mime::BYTE_STREAM {
            MessageType::Binary
        } else {
            unreachable!()
        };
        Ok(Self {
            sink: Box::pin(sink::unfold(
                (conn.clone(), ty),
                |(conn, accept), msg| async move {
                    conn.send(msg).await?;
                    Ok((conn, accept))
                },
            )),
            conn,
            accept: ty,
            _phantom: Default::default(),
        })
    }

    /// Project a `Pin<&mut Self>` to a pinned reference to the underlying connection.
    fn pinned_inner(self: Pin<&mut Self>) -> Pin<&mut WebSocketConnection> {
        // # Soundness
        //
        // This implements _structural pinning_ for [Connection]. This comes with some requirements
        // to maintain safety, as described at
        // https://doc.rust-lang.org/std/pin/index.html#pinning-is-structural-for-field:
        //
        // 1. The struct must only be [Unpin] if all the structural fields are [Unpin]. This is the
        //    default, and we don't explicitly implement [Unpin] for [Connection].
        // 2. The destructor of the struct must not move structural fields out of its argument. This
        //    is enforced by the compiler in our [Drop] implementation, which follows the idiom for
        //    safe [Drop] implementations for pinned structs.
        // 3. You must make sure that you uphold the [Drop] guarantee: once your struct is pinned,
        //    the memory that contains the content is not overwritten or deallocated without calling
        //    the content’s destructors. This is also enforced by our [Drop] implementation.
        // 4. You must not offer any other operations that could lead to data being moved out of the
        //    structural fields when your type is pinned. There are no operations on this type that
        //    move out of `conn`.
        unsafe { self.map_unchecked_mut(|s| &mut s.conn) }
    }
}

pub(crate) type Handler<State, Error> = Box<
    dyn 'static
        + Send
        + Sync
        + Fn(RequestParams, WebSocketConnection, &State) -> BoxFuture<Result<(), SocketError<Error>>>,
>;

pub(crate) fn handler<State, Error, ToClient, FromClient, F, VER: StaticVersionType>(
    f: F,
) -> Handler<State, Error>
where
    F: 'static
        + Send
        + Sync
        + Fn(
            RequestParams,
            Connection<ToClient, FromClient, Error, VER>,
            &State,
        ) -> BoxFuture<Result<(), Error>>,
    State: 'static + Send + Sync,
    ToClient: 'static + Serialize + ?Sized,
    FromClient: 'static + DeserializeOwned,
    Error: 'static + Send + Display,
{
    raw_handler(move |req, conn, state| {
        f(req, conn, state)
            .map_err(SocketError::AppSpecific)
            .boxed()
    })
}

struct StreamHandler<F, VER: StaticVersionType>(F, PhantomData<VER>);

impl<F, VER: StaticVersionType> StreamHandler<F, VER> {
    fn handle<'a, State, Error, Msg>(
        &self,
        req: RequestParams,
        mut conn: Connection<Msg, (), Error, VER>,
        state: &'a State,
    ) -> BoxFuture<'a, Result<(), SocketError<Error>>>
    where
        F: 'static + Send + Sync + Fn(RequestParams, &State) -> BoxStream<Result<Msg, Error>>,
        State: 'static + Send + Sync,
        Msg: 'static + Serialize + Send + Sync,
        Error: 'static + Send,
        VER: 'static + Send + Sync,
    {
        let mut stream = (self.0)(req, state);
        async move {
            while let Some(msg) = stream.next().await {
                conn.send(&msg.map_err(SocketError::AppSpecific)?).await?;
            }
            Ok(())
        }
        .boxed()
    }
}

pub(crate) fn stream_handler<State, Error, Msg, F, VER>(f: F) -> Handler<State, Error>
where
    F: 'static + Send + Sync + Fn(RequestParams, &State) -> BoxStream<Result<Msg, Error>>,
    State: 'static + Send + Sync,
    Msg: 'static + Serialize + Send + Sync,
    Error: 'static + Send + Display,
    VER: 'static + Send + Sync + StaticVersionType,
{
    let handler: StreamHandler<F, VER> = StreamHandler(f, Default::default());
    raw_handler(move |req, conn, state| handler.handle(req, conn, state))
}

fn raw_handler<State, Error, ToClient, FromClient, F, VER>(f: F) -> Handler<State, Error>
where
    F: 'static
        + Send
        + Sync
        + Fn(
            RequestParams,
            Connection<ToClient, FromClient, Error, VER>,
            &State,
        ) -> BoxFuture<Result<(), SocketError<Error>>>,
    State: 'static + Send + Sync,
    ToClient: 'static + Serialize + ?Sized,
    FromClient: 'static + DeserializeOwned,
    Error: 'static + Send + Display,
    VER: StaticVersionType,
{
    let close = |conn: WebSocketConnection, res: Result<(), SocketError<Error>>| async move {
        // When the handler finishes, send a close message. If there was an error, include the error
        // message.
        let msg = res.as_ref().err().map(|err| CloseFrame {
            code: err.code(),
            reason: Cow::Owned(err.to_string()),
        });
        conn.send(Message::Close(msg)).await?;
        res
    };
    Box::new(move |req, raw_conn, state| {
        let accept = match req.accept() {
            Ok(accept) => accept,
            Err(err) => return close(raw_conn, Err(err.into())).boxed(),
        };
        let conn = match Connection::new(&accept, raw_conn.clone()) {
            Ok(conn) => conn,
            Err(err) => return close(raw_conn, Err(err)).boxed(),
        };
        f(req, conn, state)
            .then(move |res| close(raw_conn, res))
            .boxed()
    })
}

struct MapErr<State, Error, F> {
    handler: Handler<State, Error>,
    map: Arc<F>,
}

impl<State, Error, F> MapErr<State, Error, F> {
    fn handle<'a, Error2>(
        &self,
        req: RequestParams,
        conn: WebSocketConnection,
        state: &'a State,
    ) -> BoxFuture<'a, Result<(), SocketError<Error2>>>
    where
        F: 'static + Send + Sync + Fn(Error) -> Error2,
        State: 'static + Send + Sync,
        Error: 'static,
    {
        let map = self.map.clone();
        let fut = (self.handler)(req, conn, state);
        async move { fut.await.map_err(|err| err.map_app_specific(&*map)) }.boxed()
    }
}

pub(crate) fn map_err<State, Error, Error2>(
    h: Handler<State, Error>,
    f: impl 'static + Send + Sync + Fn(Error) -> Error2,
) -> Handler<State, Error2>
where
    State: 'static + Send + Sync,
    Error: 'static,
{
    let handler = MapErr {
        handler: h,
        map: Arc::new(f),
    };
    Box::new(move |req, conn, state| handler.handle(req, conn, state))
}
