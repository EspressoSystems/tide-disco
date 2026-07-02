// Copyright (c) 2022 Espresso Systems (espressosys.com)
// This file is part of the tide-disco library.

// You should have received a copy of the MIT License
// along with the tide-disco library. If not, see <https://mit-license.org/>.

//! Interfaces for methods of accessing to state.
//!
//! A common pattern is for the `State` type of an [App](crate::App) to enable some form of interior
//! mutability, with the option of read-only access, such as [RwLock]. Route handlers will then
//! acquire either shared immutable access or exclusive mutable access, where the access mutability
//! is linked to the HTTP method of the route -- a GET method implies immutable access, for example,
//! while a POST method implies mutable access.
//!
//! [tide-disco](crate) supports this pattern ergonomically for any state type which has a notion of
//! reading and writing. This module defines the traits [ReadState] and [WriteState] for states
//! which support immutable and mutable access, respectively, and implements the traits for commonly
//! used types such as [RwLock], [Mutex], and [Arc]. Of course, you are free to implement these
//! traits yourself if your state type has some other notion of shared access or interior
//! mutability.

use crate::http;
use async_std::sync::{Arc, Mutex, RwLock};
use async_trait::async_trait;
use futures::future::BoxFuture;
use std::fmt::{self, Display, Formatter};
use std::str::FromStr;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Method {
    Http(http::Method),
    Socket,
    Metrics,
}

impl Method {
    /// The HTTP GET method.
    pub fn get() -> Self {
        Self::Http(http::Method::Get)
    }

    /// The HTTP POST method.
    pub fn post() -> Self {
        Self::Http(http::Method::Post)
    }

    /// The HTTP PUT method.
    pub fn put() -> Self {
        Self::Http(http::Method::Put)
    }

    /// The HTTP DELETE method.
    pub fn delete() -> Self {
        Self::Http(http::Method::Delete)
    }

    /// The Tide Disco SOCKET method.
    pub fn socket() -> Self {
        Self::Socket
    }

    /// The Tide Disco METRICS method.
    pub fn metrics() -> Self {
        Self::Metrics
    }

    /// Check if a method is a standard HTTP method.
    pub fn is_http(&self) -> bool {
        matches!(self, Self::Http(_))
    }

    /// Check if a request method implies mutable access to the state.
    pub fn is_mutable(&self) -> bool {
        match self {
            Self::Http(m) => !m.is_safe(),
            Self::Socket => true,
            Self::Metrics => false,
        }
    }
}

impl From<http::Method> for Method {
    fn from(m: http::Method) -> Self {
        Self::Http(m)
    }
}

impl Display for Method {
    fn fmt(&self, f: &mut Formatter) -> fmt::Result {
        match self {
            Self::Http(m) => write!(f, "{}", m),
            Self::Socket => write!(f, "SOCKET"),
            Self::Metrics => write!(f, "METRICS"),
        }
    }
}

pub struct ParseMethodError;

impl FromStr for Method {
    type Err = ParseMethodError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "SOCKET" => Ok(Self::Socket),
            "METRICS" => Ok(Self::Metrics),
            _ => s.parse().map_err(|_| ParseMethodError).map(Self::Http),
        }
    }
}

/// A state which allows read access.
///
/// Implementations may allow _shared_ read access (for instance, [RwLock]) but some implementations
/// do not (for instance, [Mutex]). Therefore, do not assume that [read](Self::read) is reentrant,
/// or you may have a deadlock.
#[async_trait]
pub trait ReadState {
    /// The type of state which this type allows a caller to read.
    type State: 'static;

    /// Do an operation with immutable access to the state.
    ///
    /// # Limitations
    ///
    /// The reason this function takes a visitor function to apply to the state, rather than just
    /// returning a reference to the state, is to allow implementations that cannot provide a plain
    /// reference directly. For example, [RwLock] can produce a smart reference, a
    /// [RwLockReadGuard](async_std::sync::RwLockReadGuard), but not a plain reference.
    ///
    /// Note that GATs may allow us to make this interface more ergonomic in the future. With stable
    /// GATs, this trait could be written like
    ///
    /// ```ignore
    /// trait ReadState {
    ///     type State: 'static;
    ///     type Reference<'a>: 'a + Deref<Target = Self::State>;
    ///     fn read(&self) -> Self::Reference<'_>;
    /// }
    /// ```
    ///
    /// [Like many function parameters](crate#boxed-futures) in [tide_disco](crate), the
    /// function to apply to the state is also required to return a _boxed_ future.
    async fn read<T>(
        &self,
        op: impl Send + for<'a> FnOnce(&'a Self::State) -> BoxFuture<'a, T> + 'async_trait,
    ) -> T;
}

/// A state which allows exclusive, write access.
#[async_trait]
pub trait WriteState: ReadState {
    /// Do an operation with mutable access to the state.
    ///
    /// # Limitations
    ///
    /// The reason this function takes a visitor function to apply to the state, rather than just
    /// returning a mutable reference to the state, is to allow implementations that cannot provide
    /// a plain mutable reference directly. For example, [RwLock] can produce a smart reference, a
    /// [RwLockWriteGuard](async_std::sync::RwLockWriteGuard), but not a plain mutable reference.
    ///
    /// Note that GATs may allow us to make this interface more ergonomic in the future. With stable
    /// GATs, this trait could be written like
    ///
    /// ```ignore
    /// trait WriteState {
    ///     type State: 'static;
    ///     type MutReference<'a>: 'a + DerefMut<Target = Self::State>;
    ///     fn write(&self) -> Self::MutReference<'_>;
    /// }
    /// ```
    ///
    /// [Like many function parameters](crate#boxed-futures) in [tide_disco](crate), the
    /// function to apply to the state is also required to return a _boxed_ future.
    async fn write<T>(
        &self,
        op: impl Send + for<'a> FnOnce(&'a mut Self::State) -> BoxFuture<'a, T> + 'async_trait,
    ) -> T;
}

#[async_trait]
impl<State: 'static + Send + Sync> ReadState for RwLock<State> {
    type State = State;
    async fn read<T>(
        &self,
        op: impl Send + for<'a> FnOnce(&'a Self::State) -> BoxFuture<'a, T> + 'async_trait,
    ) -> T {
        op(&*self.read().await).await
    }
}

#[async_trait]
impl<State: 'static + Send + Sync> WriteState for RwLock<State> {
    async fn write<T>(
        &self,
        op: impl Send + for<'a> FnOnce(&'a mut Self::State) -> BoxFuture<'a, T> + 'async_trait,
    ) -> T {
        op(&mut *self.write().await).await
    }
}

#[async_trait]
impl<State: 'static + Send + Sync> ReadState for Mutex<State> {
    type State = State;
    async fn read<T>(
        &self,
        op: impl Send + for<'a> FnOnce(&'a Self::State) -> BoxFuture<'a, T> + 'async_trait,
    ) -> T {
        op(&*self.lock().await).await
    }
}

#[async_trait]
impl<State: 'static + Send + Sync> WriteState for Mutex<State> {
    async fn write<T>(
        &self,
        op: impl Send + for<'a> FnOnce(&'a mut Self::State) -> BoxFuture<'a, T> + 'async_trait,
    ) -> T {
        op(&mut *self.lock().await).await
    }
}

#[async_trait]
impl<R: Send + Sync + ReadState> ReadState for Arc<R> {
    type State = R::State;
    async fn read<T>(
        &self,
        op: impl Send + for<'a> FnOnce(&'a Self::State) -> BoxFuture<'a, T> + 'async_trait,
    ) -> T {
        (**self).read(op).await
    }
}

#[async_trait]
impl<W: Send + Sync + WriteState> WriteState for Arc<W> {
    async fn write<T>(
        &self,
        op: impl Send + for<'a> FnOnce(&'a mut Self::State) -> BoxFuture<'a, T> + 'async_trait,
    ) -> T {
        (**self).write(op).await
    }
}

/// This allows you to do `api.get(...)` in a simple API where the state is `()`.
#[async_trait]
impl ReadState for () {
    type State = ();
    async fn read<T>(
        &self,
        op: impl Send + for<'a> FnOnce(&'a Self::State) -> BoxFuture<'a, T> + 'async_trait,
    ) -> T {
        op(&()).await
    }
}

/// This allows you to do `api.post(...)` in a simple API where the state is `()`.
#[async_trait]
impl WriteState for () {
    async fn write<T>(
        &self,
        op: impl Send + for<'a> FnOnce(&'a mut Self::State) -> BoxFuture<'a, T> + 'async_trait,
    ) -> T {
        op(&mut ()).await
    }
}
