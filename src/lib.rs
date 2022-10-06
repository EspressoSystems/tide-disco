//! _Tide Disco is a web server framework with built-in discoverability support for
//! [Tide](https://github.com/http-rs/tide)_
//!
//! # Overview
//!
//! We say a system is _discoverable_ if guesses and mistakes regarding usage are rewarded with
//! relevant documentation and assistance at producing correct requests. To offer this capability in
//! a practical way, it is helpful to specify the API in data files, rather than code, so that all
//! relevant text can be edited in one concise readable specification.
//!
//! Tide Disco leverages TOML to specify
//! - Routes with typed parameters
//! - Route documentation
//! - Route error messages
//! - General documentation
//!
//! ## Goals
//!
//! - Context-sensitive help
//! - Spelling suggestions
//! - Reference documentation assembled from route documentation
//! - Forms and other user interfaces to aid in the construction of correct inputs
//! - Localization
//! - Novice and expert help
//! - Flexible route parsing, e.g. named parameters rather than positional parameters
//! - API fuzz testing automation based on parameter types
//!
//! ## Future work
//!
//! - WebSocket support
//! - Runtime control over logging
//!
//! # Getting started
//!
//! A Tide Disco app is composed of one or more _API modules_. An API module consists of a TOML
//! specification and a set of route handlers -- Rust functions -- to provide the behavior of the
//! routes defined in the TOML. You can learn the format of the TOML file by looking at the examples
//! in this crate. Once you have it, you can load it into an API description using [Api::new]:
//!
//! ```no_run
//! # fn main() -> Result<(), tide_disco::api::ApiError> {
//! use tide_disco::Api;
//! use tide_disco::error::ServerError;
//!
//! type State = ();
//! type Error = ServerError;
//!
//! let spec = toml::from_slice(&std::fs::read("/path/to/api.toml").unwrap()).unwrap();
//! let mut api = Api::<State, Error>::new(spec)?;
//! # Ok(())
//! # }
//! ```
//!
//! Once you have an [Api], you can define route handlers for any routes in your TOML specification.
//! Suppose you have the following route definition:
//!
//! ```toml
//! [route.hello]
//! PATH = ["hello"]
//! METHOD = "GET"
//! ```
//!
//! Register a handler for it like this:
//!
//! ```no_run
//! # use tide_disco::Api;
//! # fn main() -> Result<(), tide_disco::api::ApiError> {
//! # let spec = toml::from_slice(&std::fs::read("/path/to/api.toml").unwrap()).unwrap();
//! # let mut api = Api::<(), tide_disco::error::ServerError>::new(spec)?;
//! use futures::FutureExt;
//!
//! api.get("hello", |req, state| async move { Ok("Hello, world!") }.boxed())?;
//! # Ok(())
//! # }
//! ```
//!
//! See [the API reference](Api) for more details on what you can do to create an [Api].
//!
//! Once you have registered all of your route handlers, you need to register your [Api] module with
//! an [App]:
//!
//! ```no_run
//! # type State = ();
//! # type Error = tide_disco::error::ServerError;
//! # #[async_std::main] async fn main() {
//! # let spec = toml::from_slice(&std::fs::read("/path/to/api.toml").unwrap()).unwrap();
//! # let api = tide_disco::Api::<State, Error>::new(spec).unwrap();
//! use tide_disco::App;
//!
//! let mut app = App::<State, Error>::with_state(());
//! app.register_module("api", api);
//! app.serve("http://localhost:8080").await;
//! # }
//! ```
//!
//! Then you can use your application:
//!
//! ```text
//! curl http://localhost:8080/api/hello
//! ```
//!
//! # Boxed futures
//!
//! As a web server framework, Tide Disco naturally includes many interfaces that take functions as
//! arguments. For example, route handlers are registered by passing a handler function to an [Api]
//! object. Also naturally, many of these function parameters are async, which of course just means
//! that they are regular functions returning some type `F` that implements the
//! [Future](futures::Future) trait. This is all perfectly usual, but throughout the interfaces in
//! this crate, you may notice something that is a bit unusual: many of these functions are required
//! to return not just any [Future](futures::Future), but a
//! [BoxFuture](futures::future::BoxFuture). This is due to a limitation that currently exists
//! in the Rust compiler.
//!
//! The problem arises with functions where the returned future is not `'static`, but rather borrows
//! from the function parameters. Consider the following route definition, for example:
//!
//! ```ignore
//! type State = RwLock<u64>;
//! type Error = ();
//!
//! api.at("someroute", |_req, state: &State| async {
//!     Ok(*state.read().await)
//! })
//! ```
//!
//! The `async` block in the route handler uses the `state` reference, so the resulting future is
//! only valid for as long as the reference `state` is valid. We could write the signature of the
//! route handler like this:
//!
//! ```
//! use futures::Future;
//! use tide_disco::RequestParams;
//!
//! type State = async_std::sync::RwLock<u64>;
//! type Error = ();
//!
//! fn handler<'a>(
//!     req: RequestParams,
//!     state: &'a State,
//! ) -> impl 'a + Future<Output = Result<u64, Error>> {
//!     // ...
//!     # async { Ok(*state.read().await) }
//! }
//! ```
//!
//! Notice how we explicitly constrain the future type by the lifetime `'a` using `impl` syntax.
//! Unfortunately, while we can write a function signature like this, we cannot write a type bound
//! that uses the [Fn] trait and represents the equivalent function signature. This is a problem,
//! since interfaces like [at](Api::at) would like to consume any function-like object which
//! implements [Fn], not just static function pointers. Here is what we would _like_ to write:
//!
//! ```ignore
//! impl<State, Error> Api<State, Error> {
//!     pub fn at<F, T>(&mut self, route: &str, handler: F)
//!     where
//!         F: for<'a> Fn<(RequestParams, &'a State)>,
//!         for<'a> <F as Fn<(RequestParams, &'a State)>>::Output:
//!             'a + Future<Output = Result<T, Error>>,
//!     {...}
//! }
//! ```
//!
//! Here we are using a higher-rank trait bound on the associated type `Output` of the [Fn]
//! implementation for `F` in order to constrain the future by the lifetime `'a`, which is the
//! lifetime of the `State` reference. It is actually possible to write this function signature
//! today in unstable Rust (using the raw [Fn] trait as a bound is unstable), but even then, no
//! associated type will be able to implement the HRTB due to a bug in the compiler. This limitation
//! is described in detail in
//! [this post](https://users.rust-lang.org/t/trait-bounds-for-fn-returning-a-future-that-forwards-the-lifetime-of-the-fn-s-arguments/63275/7).
//!
//! As a workaround until this is fixed, we require the function `F` to return a concrete future
//! type with an explicit lifetime parameter: [BoxFuture](futures::future::BoxFuture). This allows
//! us to specify the lifetime constraint within the HRTB on `F` itself, rather than resorting to a
//! separate HRTB on the associated type `Output` in order to be able to name the return type of
//! `F`. Here is the actual (partial) signature of [at](Api::at):
//!
//! ```ignore
//! impl<State, Error> Api<State, Error> {
//!     pub fn at<F, T>(&mut self, route: &str, handler: F)
//!     where
//!         F: for<'a> Fn(RequestParams, &'a State) -> BoxFuture<'a, Result<T, Error>>,
//!     {...}
//! }
//! ```
//!
//! What this means for your code is that functions you pass to the Tide Disco framework must return
//! a boxed future. When passing a closure, you can simply add `.boxed()` to your `async` block,
//! like this:
//!
//! ```
//! use async_std::sync::RwLock;
//! use futures::FutureExt;
//! use tide_disco::Api;
//!
//! type State = RwLock<u64>;
//! type Error = ();
//!
//! fn define_routes(api: &mut Api<State, Error>) {
//!     api.at("someroute", |_req, state: &State| async {
//!         Ok(*state.read().await)
//!     }.boxed());
//! }
//! ```
//!
//! This also means that you cannot pass the name of an `async fn` directly, since `async` functions
//! declared with the `async fn` syntax do not return a boxed future. Instead, you can wrap the
//! function in a closure:
//!
//! ```
//! use async_std::sync::RwLock;
//! use futures::FutureExt;
//! use tide_disco::{Api, RequestParams};
//!
//! type State = RwLock<u64>;
//! type Error = ();
//!
//! async fn handler(_req: RequestParams, state: &State) -> Result<u64, Error> {
//!     Ok(*state.read().await)
//! }
//!
//! fn register(api: &mut Api<State, Error>) {
//!     api.at("someroute", |req, state: &State| handler(req, state).boxed());
//! }
//! ```
//!
//! In the future, we may create an attribute macro which can rewrite an `async fn` to return a
//! boxed future directly, like
//!
//! ```ignore
//! #[boxed_future]
//! async fn handler(_req: RequestParams, state: &State) -> Result<u64, Error> {
//!     Ok(*state.read().await)
//! }
//! ```
//!

use crate::ApiKey::*;
use async_std::sync::{Arc, RwLock};
use async_std::task::sleep;
use clap::CommandFactory;
use config::{Config, ConfigError};
use routefinder::Router;
use serde::Deserialize;
use std::fs::{read_to_string, OpenOptions};
use std::io::Write;
use std::str::FromStr;
use std::time::Duration;
use std::{
    collections::HashMap,
    env,
    path::{Path, PathBuf},
};
use strum_macros::{AsRefStr, EnumString};
use tagged_base64::TaggedBase64;
use tide::http::mime;
use toml::value::Value;
use tracing::{error, trace};
use url::Url;

pub mod api;
pub mod app;
pub mod error;
pub mod healthcheck;
pub mod method;
pub mod request;
pub mod route;
pub mod socket;

pub use api::Api;
pub use app::App;
pub use error::Error;
pub use method::Method;
pub use request::{RequestError, RequestParam, RequestParamType, RequestParamValue, RequestParams};
pub use tide::http::{self, StatusCode};

pub type Html = maud::Markup;

/// Number of times to poll before failing
pub const SERVER_STARTUP_RETRIES: u64 = 255;

/// Number of milliseconds to sleep between attempts
pub const SERVER_STARTUP_SLEEP_MS: u64 = 100;

#[derive(clap::Args, Debug)]
#[clap(author, version, about, long_about = None)]
pub struct DiscoArgs {
    #[clap(long)]
    /// Server address
    pub base_url: Option<Url>,
    #[clap(long)]
    /// HTTP routes
    pub api_toml: Option<PathBuf>,
    /// If true, log in color. Otherwise, no color.
    #[clap(long)]
    pub ansi_color: Option<bool>,
}

/// Configuration keys for Tide Disco settings
///
/// The application is expected to define additional keys. Note, string literals could be used
/// directly, but defining an enum allows the compiler to catch typos.
#[derive(AsRefStr, Debug)]
#[allow(non_camel_case_types)]
pub enum DiscoKey {
    ansi_color,
    api_toml,
    app_toml,
    base_url,
    disco_toml,
}

#[derive(AsRefStr, Clone, Debug, Deserialize, strum_macros::Display)]
pub enum HealthStatus {
    Starting,
    Available,
    Stopping,
}

#[derive(Clone)]
pub struct ServerState<AppState> {
    pub health_status: Arc<RwLock<HealthStatus>>,
    pub app_state: AppState,
    pub router: Arc<Router<usize>>,
}

pub type AppState = Value;

pub type AppServerState = ServerState<AppState>;

#[allow(non_camel_case_types, clippy::upper_case_acronyms)]
#[derive(AsRefStr, Debug)]
enum ApiKey {
    DOC,
    METHOD,
    PATH,
    #[strum(serialize = "route")]
    ROUTE,
}

/// Check api.toml for schema compliance errors
///
/// Checks
/// - Unsupported request method
/// - Missing DOC string
/// - Route paths missing or not an array
pub fn check_api(api: toml::Value) -> bool {
    let mut error_count = 0;
    if let Some(api_map) = api[ROUTE.as_ref()].as_table() {
        let methods = vec!["GET", "POST"];
        api_map.values().for_each(|entry| {
            if let Some(paths) = entry[PATH.as_ref()].as_array() {
                let first_segment = get_first_segment(vs(&paths[0]));

                // Check the method is GET or PUT.
                let method = vk(entry, METHOD.as_ref());
                if !methods.contains(&method.as_str()) {
                    error!(
                        "Route: /{}: Unsupported method: {}. Expected one of: {:?}",
                        &first_segment, &method, &methods
                    );
                    error_count += 1;
                }

                // Check for DOC string.
                if entry.get(DOC.as_ref()).is_none() || entry[DOC.as_ref()].as_str().is_none() {
                    error!("Route: /{}: Missing DOC string.", &first_segment);
                    error_count += 1;
                }

                // Every URL segment pattern must have a valid type. For example,
                // if a segment `:amount` might have type UrlSegment::Integer
                // indicated by
                //    ":amount" = "Integer"
                // in the TOML.
                let paths = entry[PATH.as_ref()]
                    .as_array()
                    .expect("Expecting TOML array.");
                for path in paths {
                    if path.is_str() {
                        for segment in path.as_str().unwrap().split('/') {
                            if let Some(parameter) = segment.strip_prefix(':') {
                                let stype = vk(entry, segment);
                                if UrlSegment::from_str(&stype).is_err() {
                                    error!(
                                        "Route /{}: Unrecognized type {} for pattern {}.",
                                        &first_segment, stype, &parameter
                                    );
                                    error_count += 1;
                                }
                            }
                        }
                    } else {
                        error!(
                            "Route /{}: Found path '{:?}' but expecting a string.",
                            &first_segment, path
                        );
                    }
                }
            } else {
                error!("Expecting TOML array for {:?}.", &entry[PATH.as_ref()]);
                error_count += 1;
            }
        })
    }
    error_count == 0
}

/// Load the web API or panic
pub fn load_api(path: &Path) -> toml::Value {
    let messages = read_to_string(&path).unwrap_or_else(|_| panic!("Unable to read {:?}.", &path));
    let api: toml::Value =
        toml::from_str(&messages).unwrap_or_else(|_| panic!("Unable to parse {:?}.", &path));
    if !check_api(api.clone()) {
        panic!("API specification has errors.",);
    }
    api
}

/// Add routes from api.toml to the routefinder instance in tide-disco
pub fn configure_router(api: &toml::Value) -> Arc<Router<usize>> {
    let mut router = Router::new();
    if let Some(api_map) = api[ROUTE.as_ref()].as_table() {
        let mut index = 0usize;
        api_map.values().for_each(|entry| {
            let paths = entry[PATH.as_ref()]
                .as_array()
                .expect("Expecting TOML array.");
            for path in paths {
                trace!("adding path: {:?}", path);
                index += 1;
                router
                    .add(path.as_str().expect("Expecting a path string."), index)
                    .unwrap();
            }
        })
    }
    Arc::new(router)
}

/// Return a JSON expression with status 200 indicating the server
/// is up and running. The JSON expression is normally
///    {"status": "Available"}
/// When the server is running but unable to process requests
/// normally, a response with status 503 and payload {"status":
/// "unavailable"} should be added.
pub async fn healthcheck(
    req: tide::Request<AppServerState>,
) -> Result<tide::Response, tide::Error> {
    let status = req.state().health_status.read().await;
    Ok(tide::Response::builder(StatusCode::Ok)
        .content_type(mime::JSON)
        .body(tide::prelude::json!({"status": status.as_ref() }))
        .build())
}

// Get a string from a toml::Value or panic.
fn vs(v: &Value) -> &str {
    v.as_str().unwrap_or_else(|| {
        panic!(
            "Expecting TOML string, but found type {}: {:?}",
            v.type_str(),
            v
        )
    })
}

// Get a string from an array toml::Value or panic.
fn vk(v: &Value, key: &str) -> String {
    if let Some(vv) = v.get(key) {
        vv.as_str()
            .unwrap_or_else(|| {
                panic!(
                    "Expecting TOML string for {}, but found type {}",
                    key,
                    v[key].type_str()
                )
            })
            .to_string()
    } else {
        error!("No value for key {}", key);
        "<missing>".to_string()
    }
}

// Given a string delimited by slashes, get the first non-empty
// segment.
//
// For example,
// - get_first_segment("/foo/bar") -> "foo"
// - get_first_segment("first/second") -> "first"
fn get_first_segment(s: &str) -> String {
    let first_path = s.strip_prefix('/').unwrap_or(s);
    first_path
        .split_once('/')
        .unwrap_or((first_path, ""))
        .0
        .to_string()
}

#[derive(Clone, Debug, EnumString)]
pub enum UrlSegment {
    Boolean(Option<bool>),
    Hexadecimal(Option<u128>),
    Integer(Option<u128>),
    TaggedBase64(Option<TaggedBase64>),
    Literal(Option<String>),
}

impl UrlSegment {
    pub fn new(value: &str, vtype: UrlSegment) -> UrlSegment {
        match vtype {
            UrlSegment::Boolean(_) => UrlSegment::Boolean(value.parse::<bool>().ok()),
            UrlSegment::Hexadecimal(_) => {
                UrlSegment::Hexadecimal(u128::from_str_radix(value, 16).ok())
            }
            UrlSegment::Integer(_) => UrlSegment::Integer(value.parse().ok()),
            UrlSegment::TaggedBase64(_) => {
                UrlSegment::TaggedBase64(TaggedBase64::parse(value).ok())
            }
            UrlSegment::Literal(_) => UrlSegment::Literal(Some(value.to_string())),
        }
    }
    pub fn is_bound(&self) -> bool {
        match self {
            UrlSegment::Boolean(v) => v.is_some(),
            UrlSegment::Hexadecimal(v) => v.is_some(),
            UrlSegment::Integer(v) => v.is_some(),
            UrlSegment::TaggedBase64(v) => v.is_some(),
            UrlSegment::Literal(v) => v.is_some(),
        }
    }
}

/// Get the path to `api.toml`
pub fn get_api_path(api_toml: &str) -> PathBuf {
    [env::current_dir().unwrap(), api_toml.into()]
        .iter()
        .collect::<PathBuf>()
}

/// Convert the command line arguments for the config-rs crate
fn get_cmd_line_map<Args: CommandFactory>() -> config::Environment {
    config::Environment::default().source(Some({
        let mut cla = HashMap::new();
        let matches = Args::command().get_matches();
        for arg in Args::command().get_arguments() {
            if let Some(value) = matches.get_one::<String>(arg.get_id().as_str()) {
                let key = arg.get_id().as_str().replace('-', "_");
                cla.insert(key, value.to_owned());
            }
        }
        cla
    }))
}

/// Compose the path to the application's configuration file
pub fn compose_config_path(org_dir_name: &str, app_name: &str) -> PathBuf {
    let mut app_config_path = org_data_path(org_dir_name);
    app_config_path = app_config_path.join(app_name).join(app_name);
    app_config_path.set_extension("toml");
    app_config_path
}

/// Get the application configuration
///
/// Build the configuration from
/// - Defaults in the tide-disco source
/// - Defaults passed from the app
/// - A configuration file from the app
/// - Command line arguments
/// - Environment variables
/// Last one wins.
///
/// Environment variables have a prefix of the given app_name in upper case with hyphens converted
/// to underscores. Hyphens are illegal in environment variables in bash, et.al..
pub fn compose_settings<Args: CommandFactory>(
    org_name: &str,
    app_name: &str,
    app_defaults: &[(&str, &str)],
) -> Result<Config, ConfigError> {
    let app_config_file = &compose_config_path(org_name, app_name);
    {
        let app_config = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(app_config_file);
        if let Ok(mut app_config) = app_config {
            write!(
                app_config,
                "# {app_name} configuration\n\n\
                 # Note: keys must be lower case.\n\n"
            )
            .map_err(|e| ConfigError::Foreign(e.into()))?;
            for (k, v) in app_defaults {
                writeln!(app_config, "{k} = \"{v}\"")
                    .map_err(|e| ConfigError::Foreign(e.into()))?;
            }
        }
        // app_config file handle gets closed exiting this scope so
        // Config can read it.
    }
    let env_var_prefix = &app_name.replace('-', "_");
    let org_config_file = org_data_path(org_name).join("org.toml");
    // In the config-rs crate, environment variable names are converted to lower case, but keys in
    // files are not, so if we want environment variables to override file value, we must make file
    // keys lower case. This is a config-rs bug. See https://github.com/mehcode/config-rs/issues/340
    let mut builder = Config::builder()
        .set_default(DiscoKey::base_url.as_ref(), "http://localhost:65535")?
        .set_default(DiscoKey::disco_toml.as_ref(), "disco.toml")?
        .set_default(
            DiscoKey::app_toml.as_ref(),
            app_api_path(org_name, app_name)
                .to_str()
                .expect("Invalid api path"),
        )?
        .set_default(DiscoKey::ansi_color.as_ref(), false)?
        .add_source(config::File::with_name("config/default.toml"))
        .add_source(config::File::with_name(
            org_config_file
                .to_str()
                .expect("Invalid organization configuration file path"),
        ))
        .add_source(config::File::with_name(
            app_config_file
                .to_str()
                .expect("Invalid application configuration file path"),
        ))
        .add_source(get_cmd_line_map::<Args>())
        .add_source(config::Environment::with_prefix(env_var_prefix)); // No hyphens allowed
    for (k, v) in app_defaults {
        builder = builder.set_default(*k, *v).expect("Failed to set default");
    }
    builder.build()
}

pub fn init_logging(want_color: bool) {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_ansi(want_color)
        .init();
}

pub fn org_data_path(org_name: &str) -> PathBuf {
    dirs::data_local_dir()
        .unwrap_or_else(|| env::current_dir().unwrap_or_else(|_| PathBuf::from("./")))
        .join(org_name)
}

pub fn app_api_path(org_name: &str, app_name: &str) -> PathBuf {
    org_data_path(org_name).join(app_name).join("api.toml")
}

/// Wait for the server to respond to a connection request
///
/// This is useful for tests for which it doesn't make sense to send requests until the server has
/// started.
pub async fn wait_for_server(url: &Url, retries: u64, sleep_ms: u64) {
    let dur = Duration::from_millis(sleep_ms);
    for _ in 0..retries {
        if surf::connect(&url).send().await.is_ok() {
            return;
        }
        sleep(dur).await;
    }
    panic!(
        "Server did not start in {:?} milliseconds",
        sleep_ms * SERVER_STARTUP_RETRIES
    );
}
