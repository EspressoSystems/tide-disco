use async_std::sync::{Arc, RwLock};
use config::ConfigError;
#[cfg(not(windows))]
use signal::InterruptHandle;
#[cfg(not(windows))]
use signal_hook::consts::{SIGINT, SIGTERM, SIGUSR1};
use std::env::current_dir;
use tide_disco::{
    app_api_path, compose_settings, configure_router, get_api_path, init_web_server, load_api,
    AppServerState, DiscoArgs, DiscoKey, HealthStatus::*,
};
use tracing::info;

mod signal;

// This demonstrates the older way of configuring the web server. What's valuable here is that it
// shows the bare bones of discoverability from a TOML file.
#[async_std::main]
async fn main() -> Result<(), ConfigError> {
    let api_path = current_dir().unwrap().join("api").join("api.toml");
    let api_path_str = api_path.to_str().unwrap();

    // Combine settings from multiple sources.
    let settings = compose_settings::<DiscoArgs>(
        "acme",
        "rocket-sleds",
        &[(DiscoKey::api_toml.as_ref(), api_path_str)],
    )?;

    // Colorful logs upon request.
    let want_color = settings
        .get_bool(DiscoKey::ansi_color.as_ref())
        .unwrap_or(false);

    // Configure logs with timestamps, no color, and settings from
    // the RUST_LOG environment variable.
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_ansi(want_color)
        .try_init()
        .unwrap();

    info!("Settings: {:?}", settings);
    info!("api_path: {:?}", api_path_str);
    info!("app_api_path: {:?}", app_api_path("acme", "rocket-sleds"));

    // Fetch the configuration values before any slow operations.
    let api_toml = &settings.get_string(DiscoKey::api_toml.as_ref())?;
    let base_url = &settings.get_string(DiscoKey::base_url.as_ref())?;

    // Load a TOML file and display something from it.
    info!("api_toml: {:?}", api_toml);
    info!("base_url: {:?}", base_url);
    info!("get_api_path: {:?}", &get_api_path(api_toml));
    let api = load_api(&get_api_path(api_toml));
    let router = configure_router(&api);

    let web_state = AppServerState {
        health_status: Arc::new(RwLock::new(Starting)),
        app_state: api,
        router,
    };

    // Demonstrate that we can read and write the web server state.
    info!("Health Status: {}", *web_state.health_status.read().await);
    *web_state.health_status.write().await = Available;

    // Activate the handler for ^C, etc.
    #[cfg(not(windows))]
    let mut interrupt_handler = InterruptHandle::new(&[SIGINT, SIGTERM, SIGUSR1]);
    init_web_server(base_url, web_state)
        .await
        .unwrap_or_else(|err| {
            panic!("Web server exited with an error: {}", err);
        })
        .await
        .unwrap();

    #[cfg(not(windows))]
    interrupt_handler.finalize().await;

    Ok(())
}
