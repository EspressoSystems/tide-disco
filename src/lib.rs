use crate::ApiKey::*;
use async_std::sync::{Arc, RwLock};
use async_std::task::spawn;
use async_std::task::JoinHandle;
use clap::CommandFactory;
use config::{Config, ConfigError};
//use edit_distance;
use routefinder::Router;
use serde::Deserialize;
use std::fs::read_to_string;
use std::str::FromStr;
use std::{
    collections::HashMap,
    env,
    path::{Path, PathBuf},
};
use strum_macros::{AsRefStr, EnumString};
use tagged_base64::TaggedBase64;
use tide::{
    http::headers::HeaderValue,
    http::mime,
    security::{CorsMiddleware, Origin},
    Request, Response, StatusCode,
};
use toml::value::Value;
use tracing::{error, info};
use url::Url;

#[derive(AsRefStr, Debug)]
#[allow(non_camel_case_types)]
pub enum ConfigKey {
    base_url,
    disco_toml,
    brand_toml,
    api_toml,
    ansi_color,
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
    FORMAT_VERSION,
    HEADING_DESCRIPTION,
    HEADING_ENTRY,
    HEADING_PARAMETERS,
    HEADING_ROUTES,
    HTML_BOTTOM,
    HTML_TOP,
    #[strum(serialize = "meta")]
    META,
    METHOD,
    MINIMAL_HTML,
    PARAMETER_NONE,
    PARAMETER_ROW,
    PARAMETER_TABLE_CLOSE,
    PARAMETER_TABLE_OPEN,
    PATH,
    ROUTE_PATH,
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
                index += 1;
                // TODO a syntax error in api.toml would panic here
                router.add(path.as_str().unwrap(), index).unwrap();
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
/*            .expect(&format!(
                "Expecting TOML string for {}, but found type {}",
                key,
                v[key].type_str()
            ))
            .to_string()
*/

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

/// Compose an HTML fragment documenting all the variations on
/// a single route
pub fn document_route(meta: &toml::Value, entry: &toml::Value) -> String {
    let mut help: String = "".into();
    let paths = entry[PATH.as_ref()]
        .as_array()
        .expect("Expecting TOML array.");
    let first_segment = get_first_segment(vs(&paths[0]));
    help += &vk(meta, HEADING_ENTRY.as_ref())
        .replace("{{METHOD}}", &vk(entry, METHOD.as_ref()))
        .replace("{{NAME}}", &first_segment);
    help += &vk(meta, HEADING_ROUTES.as_ref());
    for path in paths.iter() {
        help += &vk(meta, ROUTE_PATH.as_ref()).replace("{{PATH}}", vs(path));
    }
    help += &vk(meta, HEADING_PARAMETERS.as_ref());
    help += &vk(meta, PARAMETER_TABLE_OPEN.as_ref());
    let mut has_parameters = false;
    for (parameter, ptype) in entry
        .as_table()
        .expect("Route definitions must be tables in api.toml")
        .iter()
    {
        info!("Parameter: {:?}", &parameter);
        if let Some(parameter) = parameter.strip_prefix(':') {
            has_parameters = true;
            help += &vk(meta, PARAMETER_ROW.as_ref())
                .to_owned()
                .replace("{{NAME}}", parameter)
                .replace("{{TYPE}}", vs(ptype));
        }
    }
    if !has_parameters {
        help += &vk(meta, PARAMETER_NONE.as_ref());
    }
    help += &vk(meta, PARAMETER_TABLE_CLOSE.as_ref());
    help += &vk(meta, HEADING_DESCRIPTION.as_ref());
    help += &markdown::to_html(vk(entry, DOC.as_ref()).trim());
    help
}

/// Compose `api.toml` into HTML.
///
/// This function iterates over the routes, adding headers and HTML
/// class attributes to make a documentation page for the web API.
///
/// The results of this could be precomputed and cached.
pub async fn compose_reference_documentation(
    req: tide::Request<AppServerState>,
) -> Result<tide::Response, tide::Error> {
    let package_name = env!("CARGO_PKG_NAME");
    let package_description = env!("CARGO_PKG_DESCRIPTION");
    let api = &req.state().app_state;
    let meta = &api["meta"];
    let version = vk(meta, FORMAT_VERSION.as_ref());
    let mut help = vk(meta, HTML_TOP.as_ref())
        .replace("{{NAME}}", package_name)
        .replace("{{FORMAT_VERSION}}", &version)
        .replace("{{DESCRIPTION}}", package_description);
    if let Some(api_map) = api[ROUTE.as_ref()].as_table() {
        api_map.values().for_each(|entry| {
            help += &document_route(meta, entry);
        });
    }
    help += &format!("{}\n", &vk(meta, HTML_BOTTOM.as_ref()));
    Ok(tide::Response::builder(200)
        .content_type(tide::http::mime::HTML)
        .body(help)
        .build())
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

// TODO Associate a handler with a pattern somehow.
pub async fn disco_dispatch(
    req: Request<AppServerState>,
    bindings: HashMap<String, UrlSegment>,
) -> tide::Result {
    let title = "Valid response here";
    let body = &format!("<pre>Bindings: {:?}</pre>", bindings);
    Ok(Response::builder(StatusCode::Ok)
        .body(
            vk(&req.state().app_state[META.as_ref()], MINIMAL_HTML.as_ref())
                .replace("{{TITLE}}", title)
                .replace("{{BODY}}", body),
        )
        .content_type(mime::HTML)
        .build())
}

pub async fn disco_web_handler(req: Request<AppServerState>) -> tide::Result {
    let router = &req.state().router;
    let path = req.url().path();
    let route_match = router.best_match(path);
    let first_segment = get_first_segment(path);
    info!("url: {}, pattern: {:?}", req.url(), route_match);

    let mut body: String = "<p>Something went wrong.</p>".into();
    let mut best: String = "".into();
    let mut distance = usize::MAX;
    let api = &req.state().app_state;
    if let Some(route_match) = route_match {
        // We might have a valid match or there may be type errors in
        // the captures. Type check the captures and report any
        // failures. If the types match, dispatch to the appropriate
        // handler.
        let mut bindings = HashMap::<String, UrlSegment>::new();
        let mut parse_error = false;
        body = "".into();
        let meta = &api["meta"];
        let entry = &api[ROUTE.as_ref()].as_table().unwrap()[&first_segment];
        for capture in route_match.captures().iter() {
            let cname = ":".to_owned() + capture.name();
            // The unwrap is safe thanks to check_api().
            let vtype = api[ROUTE.as_ref()][&first_segment][cname].as_str().unwrap();
            // The unwrap is safe thanks to check_api().
            let stype = UrlSegment::from_str(vtype).unwrap();
            let binding = UrlSegment::new(capture.value(), stype);
            if !&binding.is_bound() {
                parse_error = true;
                body = format!(
                    "{}\n<p>Expecting {} for {}</p>\n",
                    body,
                    vtype,
                    capture.name(),
                );
            }
            bindings.insert(capture.name().to_string(), binding);
        }
        if parse_error {
            let template = vk(&req.state().app_state[META.as_ref()], MINIMAL_HTML.as_ref());
            let content = format!("{}{}", body, &document_route(meta, entry));
            let body = template
                .replace("{{TITLE}}", "Route syntax error")
                .replace("{{BODY}}", &content);
            Ok(Response::builder(StatusCode::NotFound)
                .body(body)
                .content_type(mime::HTML)
                .build())
        } else {
            disco_dispatch(req, bindings).await
        }
    } else {
        // No pattern matched. Note, no wildcards were added, so now
        // we fuzzy match and give closest help
        // - Does the first segment match?
        // - Is the first segment spelled incorrectly?
        let meta = &api["meta"];
        if let Some(api_map) = api[ROUTE.as_ref()].as_table() {
            api_map.keys().for_each(|entry| {
                let d = edit_distance::edit_distance(&first_segment, entry);
                if d < distance {
                    (best, distance) = (entry.into(), d);
                }
            });
            body = if 0 < distance {
                format!(
                    "<p>No exact match for /{}. Closet match is /{}.</p>\n{}",
                    &first_segment,
                    &best,
                    document_route(meta, &api[ROUTE.as_ref()][&best])
                )
            } else {
                format!(
                    "<p>Invalid arguments for /{}.</p>\n{}",
                    &first_segment,
                    document_route(meta, &api[ROUTE.as_ref()][&first_segment])
                )
            };
        }
        Ok(Response::builder(StatusCode::NotFound)
            .body(
                vk(&req.state().app_state[META.as_ref()], MINIMAL_HTML.as_ref())
                    .replace("{{TITLE}}", "Route not found")
                    .replace("{{BODY}}", &body),
            )
            .content_type(mime::HTML)
            .build())
    }
}

// TODO The routes should come from api.toml.
pub async fn init_web_server(
    base_url: &str,
    state: AppServerState,
) -> std::io::Result<JoinHandle<std::io::Result<()>>> {
    let base_url = Url::parse(base_url).unwrap();
    let mut web_server = tide::with_state(state);
    web_server.with(
        CorsMiddleware::new()
            .allow_methods("GET, POST".parse::<HeaderValue>().unwrap())
            .allow_headers("*".parse::<HeaderValue>().unwrap())
            .allow_origin(Origin::from("*"))
            .allow_credentials(true),
    );

    web_server.at("/help").get(compose_reference_documentation);
    web_server.at("/healthcheck").get(healthcheck);
    web_server.at("/").all(disco_web_handler);
    web_server.at("/*").all(disco_web_handler);
    web_server.at("/public").serve_dir("public/media/")?;

    Ok(spawn(web_server.listen(base_url.to_string())))
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
            if let Some(value) = matches.value_of(arg.get_id()) {
                let key = arg.get_id().replace('-', "_");
                cla.insert(key, value.to_owned());
            }
        }
        cla
    }))
}

/// Get the application configuration
///
/// Gets the configuration from
/// - Defaults in the source
/// - A configuration file config/app.toml
/// - Command line arguments
/// - Environment variables
/// Last one wins. Additional file sources can be added.
pub fn get_settings<Args: CommandFactory>() -> Result<Config, ConfigError> {
    // In the config-rs crate, environment variable names are
    // converted to lower case, but keys in files are not, so if we
    // want environment variables to override file value, we must make
    // file keys lower case. This is a config-rs bug. See
    // https://github.com/mehcode/config-rs/issues/340
    Config::builder()
        .set_default(ConfigKey::base_url.as_ref(), "http://localhost:65535")?
        .set_default(ConfigKey::disco_toml.as_ref(), "api/disco.toml")?
        .set_default(ConfigKey::brand_toml.as_ref(), "api/brand.toml")?
        .set_default(ConfigKey::api_toml.as_ref(), "api/api.toml")?
        .set_default(ConfigKey::ansi_color.as_ref(), false)?
        .add_source(config::File::with_name("config/default.toml"))
        .add_source(config::File::with_name("config/org.toml"))
        .add_source(config::File::with_name("config/app.toml"))
        .add_source(get_cmd_line_map::<Args>())
        .add_source(config::Environment::with_prefix("APP"))
        .build()
}
