use snafu::{OptionExt, Snafu};
use std::collections::HashMap;
use strum_macros::EnumString;
use tagged_base64::TaggedBase64;
use tide::http::Headers;

#[derive(Clone, Debug, Snafu)]
pub enum RequestError {
    MissingParam { param: RequestParam },
}

/// Parameters passed to a route handler.
///
/// These parameters describe the incoming request and the current server state.
#[derive(Clone, Debug)]
pub struct RequestParams {
    headers: Headers,
    post_data: Vec<u8>,
    params: HashMap<String, RequestParamValue>,
}

impl RequestParams {
    pub(crate) async fn new<S>(
        mut req: tide::Request<S>,
        formal_params: &[RequestParam],
    ) -> Result<Self, RequestError> {
        Ok(Self {
            headers: AsRef::<Headers>::as_ref(&req).clone(),
            post_data: req.body_bytes().await.unwrap(),
            params: formal_params
                .iter()
                .filter_map(|param| match RequestParamValue::new(&req, param) {
                    Ok(None) => None,
                    Ok(Some(value)) => Some(Ok((param.name.clone(), value))),
                    Err(err) => Some(Err(err)),
                })
                .collect::<Result<_, _>>()?,
        })
    }

    /// The headers of the incoming request.
    pub fn headers(&self) -> &Headers {
        &self.headers
    }

    /// Get the value of a named parameter.
    pub fn param(&self, name: &str) -> Option<&RequestParamValue> {
        self.params.get(name)
    }

    /// Get the value of a named parameter and convert it to an integer.
    pub fn integer_param(&self, name: &str) -> Option<u128> {
        self.params.get(name).and_then(|val| val.as_integer())
    }

    /// Get the value of a named parameter and convert it to a [u64].
    pub fn u64_param(&self, name: &str) -> Option<u64> {
        self.integer_param(name).and_then(|i| i.try_into().ok())
    }

    /// Get the value of a named parameter and convert it to a string.
    pub fn string_param(&self, name: &str) -> Option<String> {
        self.params.get(name).and_then(|val| val.as_string())
    }

    pub fn body_bytes(&self) -> Vec<u8> {
        self.post_data.clone()
    }
}

#[derive(Clone, Debug)]
pub enum RequestParamValue {
    Boolean(bool),
    Hexadecimal(u128),
    Integer(u128),
    TaggedBase64(TaggedBase64),
    Literal(String),
}

impl RequestParamValue {
    /// Parse a parameter from a [Request](tide::Request).
    ///
    /// Returns `Ok(Some(value))` if the parameter is present and well-formed according to `formal`,
    /// `Ok(None)` if the parameter is optional and not present, or an error if the request is
    /// required and not present, or present and malformed.
    pub fn new<S>(
        req: &tide::Request<S>,
        formal: &RequestParam,
    ) -> Result<Option<Self>, RequestError> {
        if let Ok(param) = req.param(&formal.name) {
            match formal.param_type {
                RequestParamType::Literal => {
                    Ok(Some(RequestParamValue::Literal(param.to_string())))
                }
                _ => unimplemented!(
                    "parsing String into RequestParamValue based on formal.param_type"
                ),
            }
        } else {
            unimplemented!("check for the parameter in the request body")
        }
    }

    pub fn as_string(&self) -> Option<String> {
        match self {
            Self::Literal(s) => Some(s.clone()),
            _ => {
                unimplemented!("extracting a String from other parameter types, like TaggedBase64")
            }
        }
    }

    pub fn as_integer(&self) -> Option<u128> {
        unimplemented!()
    }
}

#[derive(Clone, Copy, Debug, EnumString)]
pub enum RequestParamType {
    Boolean,
    Hexadecimal,
    Integer,
    TaggedBase64,
    Literal,
}

#[derive(Clone, Debug)]
pub struct RequestParam {
    pub name: String,
    pub param_type: RequestParamType,
    pub required: bool,
}
