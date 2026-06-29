// Copyright (c) 2022 Espresso Systems (espressosys.com)
// This file is part of the tide-disco library.

// You should have received a copy of the MIT License
// along with the tide-disco library. If not, see <https://mit-license.org/>.

use serde::{Deserialize, Serialize};
use std::fmt::{self, Display, Formatter};

/// Serializable HTTP status code.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Deserialize, Serialize)]
#[serde(try_from = "u16", into = "u16")]
pub struct StatusCode(reqwest::StatusCode);

impl TryFrom<u16> for StatusCode {
    type Error = <reqwest::StatusCode as TryFrom<u16>>::Error;

    fn try_from(code: u16) -> Result<Self, Self::Error> {
        Ok(reqwest::StatusCode::try_from(code)?.into())
    }
}

impl From<StatusCode> for u16 {
    fn from(code: StatusCode) -> Self {
        code.0.as_u16()
    }
}

impl TryFrom<StatusCode> for tide::StatusCode {
    type Error = <tide::StatusCode as TryFrom<u16>>::Error;

    fn try_from(code: StatusCode) -> Result<Self, Self::Error> {
        // Tide's status code enum does not represent all possible HTTP status codes, while the
        // source type (`reqwest::StatusCode`) does, so this conversion may fail.
        u16::from(code).try_into()
    }
}

impl From<tide::StatusCode> for StatusCode {
    fn from(code: tide::StatusCode) -> Self {
        // The source type, `tide::StatusCode`, only represents valid HTTP status codes, and the
        // destination type, `reqwest::StatusCode`, can represent all valid HTTP status codes, so
        // this conversion will never panic.
        u16::from(code).try_into().unwrap()
    }
}

impl PartialEq<tide::StatusCode> for StatusCode {
    fn eq(&self, other: &tide::StatusCode) -> bool {
        *self == Self::from(*other)
    }
}

impl PartialEq<StatusCode> for tide::StatusCode {
    fn eq(&self, other: &StatusCode) -> bool {
        StatusCode::from(*self) == *other
    }
}

impl From<StatusCode> for reqwest::StatusCode {
    fn from(code: StatusCode) -> Self {
        code.0
    }
}

impl From<reqwest::StatusCode> for StatusCode {
    fn from(code: reqwest::StatusCode) -> Self {
        Self(code)
    }
}

impl PartialEq<reqwest::StatusCode> for StatusCode {
    fn eq(&self, other: &reqwest::StatusCode) -> bool {
        *self == Self::from(*other)
    }
}

impl PartialEq<StatusCode> for reqwest::StatusCode {
    fn eq(&self, other: &StatusCode) -> bool {
        *self == Self::from(*other)
    }
}

impl Display for StatusCode {
    fn fmt(&self, f: &mut Formatter) -> fmt::Result {
        write!(f, "{}", u16::from(*self))
    }
}

impl StatusCode {
    /// Returns `true` if the status code is `1xx` range.
    ///
    /// If this returns `true` it indicates that the request was received,
    /// continuing process.
    pub fn is_informational(self) -> bool {
        self.0.is_informational()
    }

    /// Returns `true` if the status code is the `2xx` range.
    ///
    /// If this returns `true` it indicates that the request was successfully
    /// received, understood, and accepted.
    pub fn is_success(self) -> bool {
        self.0.is_success()
    }

    /// Returns `true` if the status code is the `3xx` range.
    ///
    /// If this returns `true` it indicates that further action needs to be
    /// taken in order to complete the request.
    pub fn is_redirection(self) -> bool {
        self.0.is_redirection()
    }

    /// Returns `true` if the status code is the `4xx` range.
    ///
    /// If this returns `true` it indicates that the request contains bad syntax
    /// or cannot be fulfilled.
    pub fn is_client_error(self) -> bool {
        self.0.is_client_error()
    }

    /// Returns `true` if the status code is the `5xx` range.
    ///
    /// If this returns `true` it indicates that the server failed to fulfill an
    /// apparently valid request.
    pub fn is_server_error(self) -> bool {
        self.0.is_server_error()
    }

    /// The canonical reason for a given status code
    pub fn canonical_reason(self) -> Option<&'static str> {
        self.0.canonical_reason()
    }

    pub const CONTINUE: Self = Self(reqwest::StatusCode::CONTINUE);
    pub const SWITCHING_PROTOCOLS: Self = Self(reqwest::StatusCode::SWITCHING_PROTOCOLS);
    pub const PROCESSING: Self = Self(reqwest::StatusCode::PROCESSING);
    pub const OK: Self = Self(reqwest::StatusCode::OK);
    pub const CREATED: Self = Self(reqwest::StatusCode::CREATED);
    pub const ACCEPTED: Self = Self(reqwest::StatusCode::ACCEPTED);
    pub const NON_AUTHORITATIVE_INFORMATION: Self =
        Self(reqwest::StatusCode::NON_AUTHORITATIVE_INFORMATION);
    pub const NO_CONTENT: Self = Self(reqwest::StatusCode::NO_CONTENT);
    pub const RESET_CONTENT: Self = Self(reqwest::StatusCode::RESET_CONTENT);
    pub const PARTIAL_CONTENT: Self = Self(reqwest::StatusCode::PARTIAL_CONTENT);
    pub const MULTI_STATUS: Self = Self(reqwest::StatusCode::MULTI_STATUS);
    pub const ALREADY_REPORTED: Self = Self(reqwest::StatusCode::ALREADY_REPORTED);
    pub const IM_USED: Self = Self(reqwest::StatusCode::IM_USED);
    pub const MULTIPLE_CHOICES: Self = Self(reqwest::StatusCode::MULTIPLE_CHOICES);
    pub const MOVED_PERMANENTLY: Self = Self(reqwest::StatusCode::MOVED_PERMANENTLY);
    pub const FOUND: Self = Self(reqwest::StatusCode::FOUND);
    pub const SEE_OTHER: Self = Self(reqwest::StatusCode::SEE_OTHER);
    pub const NOT_MODIFIED: Self = Self(reqwest::StatusCode::NOT_MODIFIED);
    pub const USE_PROXY: Self = Self(reqwest::StatusCode::USE_PROXY);
    pub const TEMPORARY_REDIRECT: Self = Self(reqwest::StatusCode::TEMPORARY_REDIRECT);
    pub const PERMANENT_REDIRECT: Self = Self(reqwest::StatusCode::PERMANENT_REDIRECT);
    pub const BAD_REQUEST: Self = Self(reqwest::StatusCode::BAD_REQUEST);
    pub const UNAUTHORIZED: Self = Self(reqwest::StatusCode::UNAUTHORIZED);
    pub const PAYMENT_REQUIRED: Self = Self(reqwest::StatusCode::PAYMENT_REQUIRED);
    pub const FORBIDDEN: Self = Self(reqwest::StatusCode::FORBIDDEN);
    pub const NOT_FOUND: Self = Self(reqwest::StatusCode::NOT_FOUND);
    pub const METHOD_NOT_ALLOWED: Self = Self(reqwest::StatusCode::METHOD_NOT_ALLOWED);
    pub const NOT_ACCEPTABLE: Self = Self(reqwest::StatusCode::NOT_ACCEPTABLE);
    pub const PROXY_AUTHENTICATION_REQUIRED: Self =
        Self(reqwest::StatusCode::PROXY_AUTHENTICATION_REQUIRED);
    pub const REQUEST_TIMEOUT: Self = Self(reqwest::StatusCode::REQUEST_TIMEOUT);
    pub const CONFLICT: Self = Self(reqwest::StatusCode::CONFLICT);
    pub const GONE: Self = Self(reqwest::StatusCode::GONE);
    pub const LENGTH_REQUIRED: Self = Self(reqwest::StatusCode::LENGTH_REQUIRED);
    pub const PRECONDITION_FAILED: Self = Self(reqwest::StatusCode::PRECONDITION_FAILED);
    pub const PAYLOAD_TOO_LARGE: Self = Self(reqwest::StatusCode::PAYLOAD_TOO_LARGE);
    pub const URI_TOO_LONG: Self = Self(reqwest::StatusCode::URI_TOO_LONG);
    pub const UNSUPPORTED_MEDIA_TYPE: Self = Self(reqwest::StatusCode::UNSUPPORTED_MEDIA_TYPE);
    pub const RANGE_NOT_SATISFIABLE: Self = Self(reqwest::StatusCode::RANGE_NOT_SATISFIABLE);
    pub const EXPECTATION_FAILED: Self = Self(reqwest::StatusCode::EXPECTATION_FAILED);
    pub const IM_A_TEAPOT: Self = Self(reqwest::StatusCode::IM_A_TEAPOT);
    pub const MISDIRECTED_REQUEST: Self = Self(reqwest::StatusCode::MISDIRECTED_REQUEST);
    pub const UNPROCESSABLE_ENTITY: Self = Self(reqwest::StatusCode::UNPROCESSABLE_ENTITY);
    pub const LOCKED: Self = Self(reqwest::StatusCode::LOCKED);
    pub const FAILED_DEPENDENCY: Self = Self(reqwest::StatusCode::FAILED_DEPENDENCY);
    pub const UPGRADE_REQUIRED: Self = Self(reqwest::StatusCode::UPGRADE_REQUIRED);
    pub const PRECONDITION_REQUIRED: Self = Self(reqwest::StatusCode::PRECONDITION_REQUIRED);
    pub const TOO_MANY_REQUESTS: Self = Self(reqwest::StatusCode::TOO_MANY_REQUESTS);
    pub const REQUEST_HEADER_FIELDS_TOO_LARGE: Self =
        Self(reqwest::StatusCode::REQUEST_HEADER_FIELDS_TOO_LARGE);
    pub const UNAVAILABLE_FOR_LEGAL_REASONS: Self =
        Self(reqwest::StatusCode::UNAVAILABLE_FOR_LEGAL_REASONS);
    pub const INTERNAL_SERVER_ERROR: Self = Self(reqwest::StatusCode::INTERNAL_SERVER_ERROR);
    pub const NOT_IMPLEMENTED: Self = Self(reqwest::StatusCode::NOT_IMPLEMENTED);
    pub const BAD_GATEWAY: Self = Self(reqwest::StatusCode::BAD_GATEWAY);
    pub const SERVICE_UNAVAILABLE: Self = Self(reqwest::StatusCode::SERVICE_UNAVAILABLE);
    pub const GATEWAY_TIMEOUT: Self = Self(reqwest::StatusCode::GATEWAY_TIMEOUT);
    pub const HTTP_VERSION_NOT_SUPPORTED: Self =
        Self(reqwest::StatusCode::HTTP_VERSION_NOT_SUPPORTED);
    pub const VARIANT_ALSO_NEGOTIATES: Self = Self(reqwest::StatusCode::VARIANT_ALSO_NEGOTIATES);
    pub const INSUFFICIENT_STORAGE: Self = Self(reqwest::StatusCode::INSUFFICIENT_STORAGE);
    pub const LOOP_DETECTED: Self = Self(reqwest::StatusCode::LOOP_DETECTED);
    pub const NOT_EXTENDED: Self = Self(reqwest::StatusCode::NOT_EXTENDED);
    pub const NETWORK_AUTHENTICATION_REQUIRED: Self =
        Self(reqwest::StatusCode::NETWORK_AUTHENTICATION_REQUIRED);
}

#[cfg(test)]
mod test {
    use super::*;
    use vbs::{version::StaticVersion, BinarySerializer, Serializer};

    type SerializerV01 = Serializer<StaticVersion<0, 1>>;
    #[test]
    fn test_status_code() {
        for code in 100u16.. {
            // Iterate over all valid status codes, then break.
            let Ok(status) = StatusCode::try_from(code) else {
                break;
            };
            // Test type conversions.
            if let Ok(tide_status) = tide::StatusCode::try_from(code) {
                assert_eq!(tide_status, tide::StatusCode::try_from(status).unwrap());
            }
            assert_eq!(
                reqwest::StatusCode::from_u16(code).unwrap(),
                reqwest::StatusCode::from(status)
            );
            assert_eq!(code, u16::from(status));

            // Test binary round trip.
            assert_eq!(
                status,
                SerializerV01::deserialize::<StatusCode>(
                    &SerializerV01::serialize(&status).unwrap()
                )
                .unwrap()
            );

            // Test JSON round trip, readability, and backwards compatibility.
            let json = serde_json::to_string(&status).unwrap();
            assert_eq!(status, serde_json::from_str::<StatusCode>(&json).unwrap());
            assert_eq!(json, code.to_string());
            if let Ok(tide_status) = tide::StatusCode::try_from(status) {
                assert_eq!(json, serde_json::to_string(&tide_status).unwrap());
            }

            // Test display.
            assert_eq!(status.to_string(), code.to_string());
            if let Ok(tide_status) = tide::StatusCode::try_from(status) {
                assert_eq!(status.to_string(), tide_status.to_string());
            }

            // Test equality.
            if let Ok(tide_status) = tide::StatusCode::try_from(status) {
                assert_eq!(status, tide_status);
            }
            assert_eq!(status, reqwest::StatusCode::from(status));
        }

        // Now iterate over all valid _Tide_ status codes, and ensure the ycan be converted to our
        // `StatusCode`.
        for code in 100u16.. {
            let Ok(status) = tide::StatusCode::try_from(code) else {
                break;
            };
            assert_eq!(
                StatusCode::try_from(code).unwrap(),
                StatusCode::from(status)
            );
        }

        // Now iterate over all valid _reqwest_ status codes, and ensure the ycan be converted to
        // our `StatusCode`.
        for code in 100u16.. {
            let Ok(status) = reqwest::StatusCode::from_u16(code) else {
                break;
            };
            assert_eq!(
                StatusCode::try_from(code).unwrap(),
                StatusCode::from(status)
            );
        }
    }
}
