use async_std::prelude::*;
use async_trait::async_trait;
use libc::c_int;
use signal_hook_async_std::{Handle, Signals};
use std::borrow::Borrow;

pub struct InterruptHandle {
    pub handle: Handle,
    pub task: Option<async_std::task::JoinHandle<()>>,
}

#[async_trait]
pub trait Interrupt {
    fn signal_action(signal: i32);
}

impl InterruptHandle {
    pub fn new<I, S>(signals: I) -> InterruptHandle
    where
        I: IntoIterator<Item = S>,
        S: Borrow<c_int>,
        Self: Sized + Send + 'static,
    {
        let signals = Signals::new(signals).expect("Failed to create signals.");

        InterruptHandle {
            handle: signals.handle(),
            task: Some(async_std::task::spawn(Self::process_signals(signals))),
        }
    }

    /// Wait for a signal and exit
    pub async fn process_signals(mut signals: Signals) {
        while let Some(signal) = signals.next().await {
            Self::signal_action(signal)
        }
    }

    pub async fn finalize(&mut self) {
        // Stop waiting for an interrupt.
        self.handle.close();

        // TODO https://github.com/EspressoSystems/tide-disco/issues/57
        if let Some(task) = &mut self.task.take() {
            task.await
        }
    }
}
