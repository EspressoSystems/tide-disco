use async_std::prelude::*;
use async_trait::async_trait;
use libc::c_int;
use signal_hook_async_std::{Handle, Signals};
use std::borrow::Borrow;

pub struct InterruptHandle {
    pub handle: Handle,
    pub task: async_std::task::JoinHandle<()>,
}

#[async_trait]
pub trait Interrupt {
    fn new<I, S>(signals: I) -> InterruptHandle
    where
        I: IntoIterator<Item = S>,
        S: Borrow<c_int>,
        Self: Sized + Send + 'static,
    {
        let signals = Signals::new(signals).expect("Failed to create signals.");

        InterruptHandle {
            handle: signals.handle(),
            task: async_std::task::spawn(Self::process_signals(signals)),
        }
    }

    /// Wait for a signal and exit
    async fn process_signals(mut signals: Signals) {
        while let Some(signal) = signals.next().await {
            Self::signal_action(signal)
        }
    }

    fn signal_action(signal: i32);

    async fn finalize(&self) {
        // Stop waiting for an interrupt.
        self.handle.close();

        // TODO This is here to mimic the example in the documentation, but
        // it doesn't seem to make any difference whether or not it's here. Why?
        // https://docs.rs/signal-hook-async-std/latest/signal_hook_async_std/
        self.task.await;
    }
}
