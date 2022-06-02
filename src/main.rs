use crate::signal::Interrupt;
use async_std::sync::{Arc, RwLock};
use clap::Parser;
use config::ConfigError;
use signal::InterruptHandle;
use signal_hook::consts::{SIGINT, SIGTERM, SIGUSR1};
use std::{path::PathBuf, process};
use tide_disco::{
    exercise_router, get_api_path, get_settings, init_web_server, load_api, AppServerState,
    HealthStatus::*,
};
use tracing::info;
use url::Url;

mod signal;

#[derive(Parser, Debug)]
#[clap(author, version, about, long_about = None)]
struct Args {
    #[clap(long)]
    base_url: Option<Url>,
    #[clap(long)]
    api_toml: Option<PathBuf>,
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
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .try_init()
        .unwrap();

    let settings = get_settings::<Args>()?;
    info!("{:?}", settings);

    // Fetch the configuration values before any slow operations.
    let api_toml = &settings.get_string("api_toml")?;
    let base_url = &settings.get_string("base_url")?;

    exercise_router();

    // Load a TOML file and display something from it.
    let api = load_api(&get_api_path(api_toml));
    info!(
        "API version: {}",
        api["meta"]["FORMAT_VERSION"]
            .as_str()
            .expect("Expecting string.")
    );

    let web_state = AppServerState {
        health_status: Arc::new(RwLock::new(Starting)),
        app_state: api,
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
// TODO CSS
// TODO add favicon.ico
// TODO Web form
// TODO keys for set_default have no typo checking.
// TODO include timestamp in logs
// TODO make tide logs one line each
