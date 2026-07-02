use serde::{Deserialize, Serialize};
use snafu::Snafu;
use strum_macros::EnumString;

#[derive(Clone, Debug, Snafu, Deserialize, Serialize)]
#[snafu(visibility(pub))]
pub enum RequestError {
    #[snafu(display("missing required parameter: {}", name))]
    MissingParam { name: String },

    #[snafu(display(
        "incorrect parameter type: {} cannot be converted to {}",
        actual,
        expected
    ))]
    IncorrectParamType {
        actual: RequestParamType,
        expected: RequestParamType,
    },

    #[snafu(display("value {} is too large for type {}", value, expected))]
    IntegerOverflow { value: u128, expected: String },

    #[snafu(display("Unable to deserialize from JSON"))]
    Json,

    #[snafu(display("Unable to deserialize from binary"))]
    Binary,

    #[snafu(display("Unable to deserialise from tagged base 64: {}", reason))]
    TaggedBase64 { reason: String },

    #[snafu(display("Content type not specified or type not supported"))]
    UnsupportedContentType,

    #[snafu(display("HTTP protocol error: {}", reason))]
    Http { reason: String },

    #[snafu(display("error parsing {} parameter: {}", param_type, reason))]
    InvalidParam { param_type: String, reason: String },

    #[snafu(display("unexpected tag in TaggedBase64: {} (expected {})", actual, expected))]
    TagMismatch { actual: String, expected: String },
}

#[derive(
    Clone, Copy, Debug, EnumString, strum_macros::Display, Deserialize, Serialize, PartialEq, Eq,
)]
pub enum RequestParamType {
    Boolean,
    Hexadecimal,
    Integer,
    TaggedBase64,
    Literal,
}
