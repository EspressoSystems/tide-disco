use async_std::prelude::*;
use libc::c_int;
use signal_hook_async_std::{Handle, Signals};
use std::borrow::Borrow;
use std::process;

pub fn report_and_exit(signal: i32) {
    println!("\nReceived signal {}", signal);
    process::exit(1);
}

/// Wait for a signal and exit
pub async fn process_signals(mut signals: Signals) {
    while let Some(signal) = signals.next().await {
        report_and_exit(signal)
    }
}

pub struct InterruptHandler {
    pub handle: Handle,
    pub task: async_std::task::JoinHandle<()>,
}

impl InterruptHandler {
    pub fn new<I, S>(signals: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Borrow<c_int>,
    {
        let signals = Signals::new(signals).expect("Failed to create signals.");

        InterruptHandler {
            handle: signals.handle(),
            task: async_std::task::spawn(process_signals(signals)),
        }
    }

    pub async fn finalize(self: Self) {
        // Stop waiting for an interrupt.
        self.handle.close();

        // TODO This is here to mimic the example in the documentation, but
        // it doesn't seem to make any difference whether or not it's here. Why?
        self.task.await;
    }
}
