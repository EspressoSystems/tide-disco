use async_std::sync::Arc;
use snafu::Snafu;
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
pub struct RequestParams<State> {
    headers: Headers,
    state: Arc<State>,
    params: HashMap<String, RequestParamValue>,
}

impl<State> RequestParams<State> {
    pub(crate) fn new<S>(
        req: &tide::Request<S>,
        state: Arc<State>,
        formal_params: &[RequestParam],
    ) -> Result<Self, RequestError> {
        Ok(Self {
            headers: AsRef::<Headers>::as_ref(req).clone(),
            state,
            params: formal_params
                .iter()
                .filter_map(|param| match RequestParamValue::new(req, param) {
                    Ok(None) => None,
                    Ok(Some(value)) => Some(Ok((param.name.clone(), value))),
                    Err(err) => Some(Err(err)),
                })
                .collect::<Result<_, _>>()?,
        })
    }

    /// The current server state.
    pub fn state(&self) -> &State {
        &*self.state
    }

    /// The headers of the incoming request.
    pub fn headers(&self) -> &Headers {
        &self.headers
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
            unimplemented!("parsing String into RequestParamValue based on formal.param_type");
        } else {
            unimplemented!("check for the parameter in the request body")
        }
    }
}

#[derive(Clone, Debug, EnumString)]
pub enum RequestParamType {
    Boolean,
    Hexadecimal,
    Integer,
    TaggedBase64,
    Literal,
}

#[derive(Clone, Debug)]
pub struct RequestParam {
    name: String,
    param_type: RequestParamType,
    required: bool,
}
