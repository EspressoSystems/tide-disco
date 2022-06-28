use async_std::task::block_on;
use snafu::{OptionExt, Snafu};
use std::collections::HashMap;
use std::fmt::Display;
use strum_macros::EnumString;
use tagged_base64::TaggedBase64;
use tide::http::{Headers, Request};
use tracing::info;

#[derive(Clone, Debug, Snafu)]
pub enum RequestError {
    #[snafu(display("missing required parameter: {}", name))]
    MissingParam { name: String },

    #[snafu(display(
        "incorrect type for parameter {}: {} cannot be converted to {}",
        name,
        param_type,
        expected
    ))]
    IncorrectParamType {
        name: String,
        param_type: RequestParamType,
        expected: String,
    },

    #[snafu(display("value {} for {} is too large for type {}", value, name, expected))]
    IntegerOverflow {
        value: u128,
        name: String,
        expected: String,
    },
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
    pub(crate) fn new<S>(
        req: &tide::Request<S>,
        formal_params: &[RequestParam],
    ) -> Result<Self, RequestError> {
        let pd = block_on(AsRef::<Request>::as_ref(req).clone().body_bytes()).unwrap();
        info!("pd: {:?}", pd);
        Ok(Self {
            headers: AsRef::<Headers>::as_ref(req).clone(),
            post_data: block_on(AsRef::<Request>::as_ref(req).clone().body_bytes()).unwrap(),
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

    /// The headers of the incoming request.
    pub fn headers(&self) -> &Headers {
        &self.headers
    }

    /// Get the value of a named parameter.
    ///
    /// The name of the parameter can be given by any type that implements [Display]. Of course, the
    /// simplest option is to use [str] or [String], as in
    ///
    /// ```
    /// # use tide_disco::*;
    /// # fn ex(req: &RequestParams) {
    /// req.param("foo")
    /// # ;}
    /// ```
    ///
    /// However, you have the option of defining a statically typed enum representing the possible
    /// parameters of a given route and using enum variants as parameter names. Among other
    /// benefits, this allows you to change the client-facing parameter names just by tweaking the
    /// [Display] implementation of your enum, without changing other code.
    ///
    /// ```
    /// use std::fmt::{self, Display, Formatter};
    ///
    /// enum RouteParams {
    ///     Param1,
    ///     Param2,
    /// }
    ///
    /// impl Display for RouteParams {
    ///     fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
    ///         let name = match self {
    ///             Self::Param1 => "param1",
    ///             Self::Param2 => "param2",
    ///         };
    ///         write!(f, "{}", name)
    ///     }
    /// }
    ///
    /// # use tide_disco::*;
    /// # fn ex(req: &RequestParams) {
    /// req.param(&RouteParams::Param1)
    /// # ;}
    /// ```
    ///
    /// You can also use [strum_macros] to automatically derive the [Display] implementation, so you
    /// only have to specify the client-facing names of each parameter:
    ///
    /// ```
    /// #[derive(strum_macros::Display)]
    /// enum RouteParams {
    ///     #[strum(serialize = "param1")]
    ///     Param1,
    ///     #[strum(serialize = "param2")]
    ///     Param2,
    /// }
    ///
    /// # use tide_disco::*;
    /// # fn ex(req: &RequestParams) {
    /// req.param(&RouteParams::Param1)
    /// # ;}
    /// ```
    ///
    /// # Errors
    ///
    /// Returns [RequestError::MissingParam] if a parameter called `name` was not provided with the
    /// request.
    ///
    /// It is recommended to implement `From<RequestError>` for the error type for your API, so that
    /// you can use `?` with this function in a route handler. If your error type implements
    /// [Error](crate::Error), you can easily use the [catch_all](crate::Error::catch_all)
    /// constructor to do this:
    ///
    /// ```
    /// use serde::{Deserialize, Serialize};
    /// use snafu::Snafu;
    /// use tide_disco::{Error, RequestError, RequestParams, StatusCode};
    ///
    /// type ApiState = ();
    ///
    /// #[derive(Debug, Snafu, Deserialize, Serialize)]
    /// struct ApiError {
    ///     status: StatusCode,
    ///     msg: String,
    /// }
    ///
    /// impl Error for ApiError {
    ///     fn catch_all(status: StatusCode, msg: String) -> Self {
    ///         Self { status, msg }
    ///     }
    ///
    ///     fn status(&self) -> StatusCode {
    ///         self.status
    ///     }
    /// }
    ///
    /// impl From<RequestError> for ApiError {
    ///     fn from(err: RequestError) -> Self {
    ///         Self::catch_all(StatusCode::BadRequest, err.to_string())
    ///     }
    /// }
    ///
    /// async fn my_route_handler(req: RequestParams, _state: &ApiState) -> Result<(), ApiError> {
    ///     let param = req.param("my_param")?;
    ///     Ok(())
    /// }
    /// ```
    pub fn param<Name>(&self, name: &Name) -> Result<&RequestParamValue, RequestError>
    where
        Name: ?Sized + Display,
    {
        self.params
            .get(&name.to_string())
            .context(MissingParamSnafu {
                name: name.to_string(),
            })
    }

    /// Get the value of a named parameter and convert it to an integer.
    ///
    /// Like [param](Self::param), but returns [None] if the parameter value cannot be converted to
    /// an integer.
    pub fn integer_param<Name>(&self, name: &Name) -> Result<u128, RequestError>
    where
        Name: ?Sized + Display,
    {
        self.param(name).and_then(|val| {
            val.as_integer().context(IncorrectParamTypeSnafu {
                name: name.to_string(),
                param_type: val.param_type(),
                expected: "Integer".to_string(),
            })
        })
    }

    /// Get the value of a named parameter and convert it to a [u64].
    ///
    /// Like [param](Self::param), but returns [None] if the parameter value cannot be converted to
    /// a [u64].
    pub fn u64_param<Name>(&self, name: &Name) -> Result<u64, RequestError>
    where
        Name: ?Sized + Display,
    {
        self.integer_param(name).and_then(|i| {
            i.try_into().ok().context(IntegerOverflowSnafu {
                name: name.to_string(),
                value: i,
                expected: "u64".to_string(),
            })
        })
    }

    /// Get the value of a named parameter and convert it to a string.
    ///
    /// Like [param](Self::param), but returns [None] if the parameter value cannot be converted to
    /// a [String].
    pub fn string_param<Name>(&self, name: &Name) -> Result<String, RequestError>
    where
        Name: ?Sized + Display,
    {
        self.param(name).and_then(|val| {
            val.as_string().context(IncorrectParamTypeSnafu {
                name: name.to_string(),
                param_type: val.param_type(),
                expected: "String".to_string(),
            })
        })
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

    pub fn param_type(&self) -> RequestParamType {
        match self {
            Self::Boolean(_) => RequestParamType::Boolean,
            Self::Hexadecimal(_) => RequestParamType::Hexadecimal,
            Self::Integer(_) => RequestParamType::Integer,
            Self::TaggedBase64(_) => RequestParamType::TaggedBase64,
            Self::Literal(_) => RequestParamType::Literal,
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

#[derive(Clone, Copy, Debug, EnumString, strum_macros::Display)]
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
