use serde::{Deserialize, Serialize};
use tide::StatusCode;

/// A response to a healthcheck endpoint.
///
/// A type implementing [HealthCheck] may be returned from a healthcheck endpoint itself (via its
/// [Serialize] implementation) as well as incorporated automatically into the global healthcheck
/// endpoint for an app. The global healthcheck will fail if any of the module healthchecks return
/// an implementation `h` of [HealthCheck] where `h.status() != StatusCode::Ok`.
///
/// We provide a standard implementation [HealthStatus] which has variants for common states an
/// application might encounter. We recommend using this implementation as a standard, although it
/// is possible to implement the [HealthCheck] trait yourself if you desire more information in
/// your healthcheck response.
pub trait HealthCheck: Serialize {
    /// The status of this health check.
    ///
    /// Should return [StatusCode::Ok] if the status is considered healthy, and some other status
    /// code if it is not.
    fn status(&self) -> StatusCode;
}

/// Common health statuses of an application.
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum HealthStatus {
    Initializing,
    Available,
    Unavailabale,
    TemporarilyUnavailable,
    Unhealthy,
    ShuttingDown,
}

impl Default for HealthStatus {
    fn default() -> Self {
        Self::Available
    }
}

impl HealthCheck for HealthStatus {
    fn status(&self) -> StatusCode {
        match self {
            // Return healthy in normal states even if the state is not `Available`, so that load
            // balances and health monitors don't kill the service while it is starting up or
            // gracefully shutting down.
            Self::Available | Self::Initializing | Self::ShuttingDown => StatusCode::Ok,
            _ => StatusCode::ServiceUnavailable,
        }
    }
}
