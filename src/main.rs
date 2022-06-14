use crate::signal::Interrupt;
use async_std::sync::{Arc, RwLock};
use clap::Parser;
use config::ConfigError;
use signal::InterruptHandle;
use signal_hook::consts::{SIGINT, SIGTERM, SIGUSR1};
use std::{path::PathBuf, process};
use tide_disco::{
    configure_router, get_api_path, get_settings, init_web_server, load_api, AppServerState,
    HealthStatus::*,
};
use tracing::info;
use url::Url;

mod signal;

#[derive(Parser, Debug)]
#[clap(author, version, about, long_about = None)]
struct Args {
    #[clap(long)]
    /// Server address
    base_url: Option<Url>,
    #[clap(long)]
    /// HTTP routes
    api_toml: Option<PathBuf>,
    /// If true, log in color. Otherwise, no color.
    #[clap(long)]
    ansi_color: Option<bool>,
}

impl Interrupt for InterruptHandle {
    fn signal_action(signal: i32) {
        // TOOD modify web_state based on the signal.
        println!("\nReceived signal {}", signal);
        process::exit(1);
    }
}

#[async_std::main]
async fn main() -> Result<(), ConfigError> {
    // Combine settings from multiple sources.
    let settings = get_settings::<Args>()?;

    // Colorful logs upon request.
    let want_color = settings.get_bool("ansi_color").unwrap_or(false);

    // Configure logs with timestamps, no color, and settings from
    // the RUST_LOG environment variable.
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_ansi(want_color)
        .try_init()
        .unwrap();

    info!("{:?}", settings);

    // Fetch the configuration values before any slow operations.
    let api_toml = &settings.get_string("api_toml")?;
    let base_url = &settings.get_string("base_url")?;

    // Load a TOML file and display something from it.
    let api = load_api(&get_api_path(api_toml));
    let router = configure_router(&api);

    let web_state = AppServerState {
        health_status: Arc::new(RwLock::new(Starting)),
        app_state: api,
        router: router,
    };

    // Demonstrate that we can read and write the web server state.
    info!("Health Status: {}", *web_state.health_status.read().await);
    *web_state.health_status.write().await = Available;

    // Activate the handler for ^C, etc.
    let mut interrupt_handler = InterruptHandle::new(&[SIGINT, SIGTERM, SIGUSR1]);
    init_web_server(&base_url, web_state)
        .await
        .unwrap_or_else(|err| {
            panic!("Web server exited with an error: {}", err);
        })
        .await
        .unwrap();

    interrupt_handler.finalize().await;

    Ok(())
}

// TODO Make the discoverability stuff work
// - Configure default routes
// - Populate our own router from api.toml
// -
// TODO Web form
// TODO keys for set_default have no typo checking.
