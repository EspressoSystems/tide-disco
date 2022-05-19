use async_std::prelude::*;
use signal_hook_async_std::Signals;
use std::process;

/// Wait for a signal and exit
pub async fn process_signals(mut signals: Signals) {
    while let Some(signal) = signals.next().await {
        println!("Received signal {:?}", signal);
        process::exit(1);
    }
}
