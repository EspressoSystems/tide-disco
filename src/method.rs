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

use crate::http::Method;
use async_std::sync::{Arc, Mutex, RwLock};
use async_trait::async_trait;
use futures::future::BoxFuture;

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

/// Check if an HTTP method implies mutable access to the state.
pub fn method_is_mutable(method: Method) -> bool {
    !method.is_safe()
}
