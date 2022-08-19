use crate::method::Method;
use ark_serialize::CanonicalDeserialize;
use jf_utils::Tagged;
use serde::{Deserialize, Serialize};
use snafu::{OptionExt, Snafu};
use std::collections::HashMap;
use std::fmt::Display;
use strum_macros::EnumString;
use tagged_base64::TaggedBase64;
use tide::http::{content::Accept, mime::Mime, Headers};

#[derive(Clone, Debug, Snafu, Deserialize, Serialize)]
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

    #[snafu(display("Unable to deserialize from JSON"))]
    Json,

    #[snafu(display("Unable to deserialize from bincode"))]
    Bincode,

    #[snafu(display("Unable to deserialize from ark format: {}", reason))]
    ArkSerialize { reason: String },

    #[snafu(display("Content type not specified or type not supported"))]
    UnsupportedContentType,

    #[snafu(display("HTTP protocol error: {}", reason))]
    Http { reason: String },

    #[snafu(display("error parsing {} parameter: {}", param_type, reason))]
    InvalidParam { param_type: String, reason: String },

    #[snafu(display("unexpected tag in TaggedBase64: {} (expected {})", actual, expected))]
    TagMismatch { actual: String, expected: String },
}

/// Parameters passed to a route handler.
///
/// These parameters describe the incoming request and the current server state.
#[derive(Clone, Debug)]
pub struct RequestParams {
    headers: Headers,
    post_data: Vec<u8>,
    params: HashMap<String, RequestParamValue>,
    method: Method,
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
            method: req.method().into(),
        })
    }

    /// The [Method] used to dispatch the request.
    pub fn method(&self) -> Method {
        self.method
    }

    /// The headers of the incoming request.
    pub fn headers(&self) -> &Headers {
        &self.headers
    }

    /// The [Accept] header of this request.
    ///
    /// The media type proposals in the resulting header are sorted in order of decreasing weight.
    ///
    /// If no [Accept] header was explicitly set, defaults to the wildcard `Accept: *`.
    ///
    /// # Error
    ///
    /// Returns [RequestError::Http] if the [Accept] header is malformed.
    pub fn accept(&self) -> Result<Accept, RequestError> {
        Self::accept_from_headers(&self.headers)
    }

    pub(crate) fn accept_from_headers(
        headers: impl AsRef<Headers>,
    ) -> Result<Accept, RequestError> {
        match Accept::from_headers(headers).map_err(|err| RequestError::Http {
            reason: err.to_string(),
        })? {
            Some(mut accept) => {
                accept.sort();
                Ok(accept)
            }
            None => {
                let mut accept = Accept::new();
                accept.set_wildcard(true);
                Ok(accept)
            }
        }
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
        self.opt_param(name).context(MissingParamSnafu {
            name: name.to_string(),
        })
    }

    /// Get the value of a named optional parameter.
    ///
    /// Like [param](Self::param), but returns [None] instead of [Err] if the parametre is missing.
    pub fn opt_param<Name>(&self, name: &Name) -> Option<&RequestParamValue>
    where
        Name: ?Sized + Display,
    {
        self.params.get(&name.to_string())
    }

    /// Get the value of a named parameter and convert it to an integer.
    ///
    /// Like [param](Self::param), but returns [Err] if the parameter value cannot be converted to
    /// an integer of the desired size.
    pub fn integer_param<Name, T>(&self, name: &Name) -> Result<T, RequestError>
    where
        Name: ?Sized + Display,
        T: TryFrom<u128>,
    {
        self.opt_integer_param(name)?.context(MissingParamSnafu {
            name: name.to_string(),
        })
    }

    /// Get the value of a named optional parameter and convert it to an integer.
    ///
    /// Like [opt_param](Self::opt_param), but returns [Err] if the parameter value cannot be
    /// converted to an integer of the desired size.
    pub fn opt_integer_param<Name, T>(&self, name: &Name) -> Result<Option<T>, RequestError>
    where
        Name: ?Sized + Display,
        T: TryFrom<u128>,
    {
        self.opt_param(name)
            .map(|val| {
                val.as_integer().context(IncorrectParamTypeSnafu {
                    name: name.to_string(),
                    param_type: val.param_type(),
                    expected: "Integer".to_string(),
                })
            })
            .transpose()
    }

    /// Get the value of a named parameter and convert it to a [bool].
    ///
    /// Like [param](Self::param), but returns [Err] if the parameter value cannot be converted to
    /// a [bool].
    pub fn boolean_param<Name>(&self, name: &Name) -> Result<bool, RequestError>
    where
        Name: ?Sized + Display,
    {
        self.opt_boolean_param(name)?.context(MissingParamSnafu {
            name: name.to_string(),
        })
    }

    /// Get the value of a named optional parameter and convert it to a [bool].
    ///
    /// Like [opt_param](Self::opt_param), but returns [Err] if the parameter value cannot be
    /// converted to a [bool].
    pub fn opt_boolean_param<Name>(&self, name: &Name) -> Result<Option<bool>, RequestError>
    where
        Name: ?Sized + Display,
    {
        self.opt_param(name)
            .map(|val| {
                val.as_boolean().context(IncorrectParamTypeSnafu {
                    name: name.to_string(),
                    param_type: val.param_type(),
                    expected: "Boolean".to_string(),
                })
            })
            .transpose()
    }

    /// Get the value of a named parameter and convert it to a string.
    ///
    /// Like [param](Self::param), but returns [Err] if the parameter value cannot be converted to
    /// a [String].
    pub fn string_param<Name>(&self, name: &Name) -> Result<&str, RequestError>
    where
        Name: ?Sized + Display,
    {
        self.opt_string_param(name)?.context(MissingParamSnafu {
            name: name.to_string(),
        })
    }

    /// Get the value of a named optional parameter and convert it to a string.
    ///
    /// Like [opt_param](Self::opt_param), but returns [Err] if the parameter value cannot be
    /// converted to a [String].
    pub fn opt_string_param<Name>(&self, name: &Name) -> Result<Option<&str>, RequestError>
    where
        Name: ?Sized + Display,
    {
        self.opt_param(name)
            .map(|val| {
                val.as_string().context(IncorrectParamTypeSnafu {
                    name: name.to_string(),
                    param_type: val.param_type(),
                    expected: "String".to_string(),
                })
            })
            .transpose()
    }

    /// Get the value of a named parameter and convert it to [TaggedBase64].
    ///
    /// Like [param](Self::param), but returns [Err] if the parameter value cannot be converted to
    /// [TaggedBase64].
    pub fn tagged_base64_param<Name>(&self, name: &Name) -> Result<&TaggedBase64, RequestError>
    where
        Name: ?Sized + Display,
    {
        self.opt_tagged_base64_param(name)?
            .context(MissingParamSnafu {
                name: name.to_string(),
            })
    }

    /// Get the value of a named optional parameter and convert it to [TaggedBase64].
    ///
    /// Like [opt_param](Self::opt_param), but returns [Err] if the parameter value cannot be
    /// converted to [TaggedBase64].
    pub fn opt_tagged_base64_param<Name>(
        &self,
        name: &Name,
    ) -> Result<Option<&TaggedBase64>, RequestError>
    where
        Name: ?Sized + Display,
    {
        self.opt_param(name)
            .map(|val| {
                val.as_tagged_base64().context(IncorrectParamTypeSnafu {
                    name: name.to_string(),
                    param_type: val.param_type(),
                    expected: "TaggedBase64".to_string(),
                })
            })
            .transpose()
    }

    /// Get the value of a named parameter and convert it to a custom type through [TaggedBase64].
    ///
    /// Like [param](Self::param), but returns [Err] if the parameter value cannot be converted to
    /// `T`.
    pub fn blob_param<Name, T>(&self, name: &Name) -> Result<T, RequestError>
    where
        Name: ?Sized + Display,
        T: Tagged + CanonicalDeserialize,
    {
        self.opt_blob_param(name)?.context(MissingParamSnafu {
            name: name.to_string(),
        })
    }

    /// Get the value of a named optional parameter and convert it to a custom type through
    /// [TaggedBase64].
    ///
    /// Like [opt_param](Self::opt_param), but returns [Err] if the parameter value cannot be
    /// converted to `T`.
    pub fn opt_blob_param<Name, T>(&self, name: &Name) -> Result<Option<T>, RequestError>
    where
        Name: ?Sized + Display,
        T: Tagged + CanonicalDeserialize,
    {
        self.opt_tagged_base64_param(name)?
            .map(|tb64| {
                if tb64.tag() == T::tag() {
                    T::deserialize(&*tb64.value()).map_err(|source| RequestError::ArkSerialize {
                        reason: source.to_string(),
                    })
                } else {
                    Err(RequestError::TagMismatch {
                        actual: tb64.tag(),
                        expected: T::tag(),
                    })
                }
            })
            .transpose()
    }

    pub fn body_bytes(&self) -> Vec<u8> {
        self.post_data.clone()
    }

    pub fn body_json<T>(&self) -> Result<T, RequestError>
    where
        T: serde::de::DeserializeOwned,
    {
        serde_json::from_slice(&self.post_data.clone()).map_err(|_| RequestError::Json {})
    }

    /// Deserialize the body of a request.
    ///
    /// The Content-Type header is used to determine the serialization format.
    pub fn body_auto<T>(&self) -> Result<T, RequestError>
    where
        T: serde::de::DeserializeOwned,
    {
        if let Some(content_type) = self.headers.get("Content-Type") {
            match content_type.as_str() {
                "application/json" => self.body_json(),
                "application/octet-stream" => {
                    let bytes = self.body_bytes();
                    bincode::deserialize(&bytes).map_err(|_err| RequestError::Bincode {})
                }
                _content_type => Err(RequestError::UnsupportedContentType {}),
            }
        } else {
            Err(RequestError::UnsupportedContentType {})
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
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
            Self::parse(param, formal).map(Some)
        } else {
            unimplemented!("check for the parameter in the request body")
        }
    }

    pub fn parse(s: &str, formal: &RequestParam) -> Result<Self, RequestError> {
        match formal.param_type {
            RequestParamType::Literal => Ok(RequestParamValue::Literal(s.to_string())),
            RequestParamType::Boolean => Ok(RequestParamValue::Boolean(s.parse().map_err(
                |err: std::str::ParseBoolError| RequestError::InvalidParam {
                    param_type: "Boolean".to_string(),
                    reason: err.to_string(),
                },
            )?)),
            RequestParamType::Integer => Ok(RequestParamValue::Integer(s.parse().map_err(
                |err: std::num::ParseIntError| RequestError::InvalidParam {
                    param_type: "Integer".to_string(),
                    reason: err.to_string(),
                },
            )?)),
            RequestParamType::Hexadecimal => Ok(RequestParamValue::Hexadecimal(
                s.parse()
                    .map_err(|err: std::num::ParseIntError| RequestError::InvalidParam {
                        param_type: "Hexadecimal".to_string(),
                        reason: err.to_string(),
                    })?,
            )),
            RequestParamType::TaggedBase64 => Ok(RequestParamValue::TaggedBase64(
                TaggedBase64::parse(s).map_err(|err| RequestError::InvalidParam {
                    param_type: "TaggedBase64".to_string(),
                    reason: err.to_string(),
                })?,
            )),
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

    pub fn as_string(&self) -> Option<&str> {
        match self {
            Self::Literal(s) => Some(s),
            _ => None,
        }
    }

    pub fn as_integer<T: TryFrom<u128>>(&self) -> Option<T> {
        match self {
            Self::Integer(x) | Self::Hexadecimal(x) => T::try_from(*x).ok(),
            _ => None,
        }
    }

    pub fn as_boolean(&self) -> Option<bool> {
        match self {
            Self::Boolean(x) => Some(*x),
            _ => None,
        }
    }

    pub fn as_tagged_base64(&self) -> Option<&TaggedBase64> {
        match self {
            Self::TaggedBase64(x) => Some(x),
            _ => None,
        }
    }
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

#[derive(Clone, Debug)]
pub struct RequestParam {
    pub name: String,
    pub param_type: RequestParamType,
    pub required: bool,
}

pub(crate) fn best_response_type(
    accept: &Accept,
    available: &[Mime],
) -> Result<Mime, RequestError> {
    // The Accept type has a `negotiate` method, but it doesn't properly handle wildcards. It
    // handles * but not */* and basetype/*, because for content type proposals like */* and
    // basetype/*, it looks for a literal match in `available`, it does not perform pattern
    // matching. So, we implement negotiation ourselves. Go through each proposed content type, in
    // the order specified by the client, and match them against our available types, respecting
    // wildcards.
    for proposed in accept.iter() {
        if proposed.basetype() == "*" {
            // The only acceptable Accept value with a basetype of * is */*, therefore this will
            // match any available type.
            return Ok(available[0].clone());
        } else if proposed.subtype() == "*" {
            // If the subtype is * but the basetype is not, look for a proposed type with a matching
            // basetype and any subtype.
            for mime in available {
                if mime.basetype() == proposed.basetype() {
                    return Ok(mime.clone());
                }
            }
        } else if available.contains(proposed) {
            // If neither part of the proposal is a wildcard, look for a literal match.
            return Ok((**proposed).clone());
        }
    }

    if accept.wildcard() {
        // If no proposals are available but a wildcard flag * was given, return any available
        // content type.
        Ok(available[0].clone())
    } else {
        Err(RequestError::UnsupportedContentType)
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::StatusCode;
    use ark_serialize::*;
    use jf_utils::tagged_blob;

    fn default_headers() -> Headers {
        let res = tide::Response::builder(StatusCode::Ok).build();
        let h: &Headers = res.as_ref();
        h.clone()
    }

    fn param(ty: RequestParamType, name: &str, val: &str) -> RequestParamValue {
        RequestParamValue::parse(
            val,
            &RequestParam {
                name: name.to_string(),
                param_type: ty,
                required: true,
            },
        )
        .unwrap()
    }

    fn request_from_params(
        params: impl IntoIterator<Item = (String, RequestParamValue)>,
    ) -> RequestParams {
        RequestParams {
            headers: default_headers(),
            post_data: Default::default(),
            params: params.into_iter().collect(),
            method: Method::get(),
        }
    }

    #[tagged_blob("BLOB")]
    #[derive(Clone, Debug, PartialEq, Eq, CanonicalSerialize, CanonicalDeserialize)]
    struct Blob {
        data: String,
    }

    #[test]
    fn test_params() {
        let tb64 = TaggedBase64::new("TAG", &[0; 20]).unwrap();
        let blob = Blob {
            data: "blob".to_string(),
        };
        let string_param = param(RequestParamType::Literal, "string", "hello");
        let integer_param = param(RequestParamType::Integer, "integer", "42");
        let boolean_param = param(RequestParamType::Boolean, "boolean", "true");
        let tagged_base64_param = param(
            RequestParamType::TaggedBase64,
            "tagged_base64",
            &tb64.to_string(),
        );
        let blob_param = param(RequestParamType::TaggedBase64, "blob", &blob.to_string());
        let params = vec![
            ("string".to_string(), string_param.clone()),
            ("integer".to_string(), integer_param.clone()),
            ("boolean".to_string(), boolean_param.clone()),
            ("tagged_base64".to_string(), tagged_base64_param.clone()),
            ("blob".to_string(), blob_param.clone()),
        ];
        let req = request_from_params(params);

        // Check untyped param.
        assert_eq!(*req.param("string").unwrap(), string_param);
        assert_eq!(*req.param("integer").unwrap(), integer_param);
        assert_eq!(*req.param("boolean").unwrap(), boolean_param);
        assert_eq!(*req.param("tagged_base64").unwrap(), tagged_base64_param);
        assert_eq!(*req.param("blob").unwrap(), blob_param);
        match req.param("nosuchparam").unwrap_err() {
            RequestError::MissingParam { name } if name == "nosuchparam" => {}
            err => panic!("expecting MissingParam {{ nosuchparam }}, got {:?}", err),
        }

        // Check untyped optional param.
        assert_eq!(*req.opt_param("string").unwrap(), string_param);
        assert_eq!(*req.opt_param("integer").unwrap(), integer_param);
        assert_eq!(*req.opt_param("boolean").unwrap(), boolean_param);
        assert_eq!(
            *req.opt_param("tagged_base64").unwrap(),
            tagged_base64_param
        );
        assert_eq!(*req.opt_param("blob").unwrap(), blob_param);
        assert_eq!(req.opt_param("nosuchparam"), None);

        // Check typed params: correct type, incorrect type, and missing cases.
        assert_eq!(req.string_param("string").unwrap(), "hello");
        match req.string_param("integer").unwrap_err() {
            RequestError::IncorrectParamType {
                name,
                param_type,
                expected,
            } if name == "integer"
                && param_type == RequestParamType::Integer
                && expected == "String" => {}
            err => panic!(
                "expecting IncorrectParamType {{ integer, Integer, String }}, got {:?}",
                err
            ),
        }
        match req.string_param("nosuchparam").unwrap_err() {
            RequestError::MissingParam { name } if name == "nosuchparam" => {}
            err => panic!("expecting MissingParam {{ nosuchparam }}, got {:?}", err),
        };

        assert_eq!(req.integer_param::<_, usize>("integer").unwrap(), 42);
        match req.integer_param::<_, usize>("string").unwrap_err() {
            RequestError::IncorrectParamType {
                name,
                param_type,
                expected,
            } if name == "string"
                && param_type == RequestParamType::Literal
                && expected == "Integer" => {}
            err => panic!(
                "expecting IncorrectParamType {{ string, Literal, Integer }}, got {:?}",
                err
            ),
        }
        match req.integer_param::<_, usize>("nosuchparam").unwrap_err() {
            RequestError::MissingParam { name } if name == "nosuchparam" => {}
            err => panic!("expecting MissingParam {{ nosuchparam }}, got {:?}", err),
        };

        assert_eq!(req.boolean_param("boolean").unwrap(), true);
        match req.boolean_param("integer").unwrap_err() {
            RequestError::IncorrectParamType {
                name,
                param_type,
                expected,
            } if name == "integer"
                && param_type == RequestParamType::Integer
                && expected == "Boolean" => {}
            err => panic!(
                "expecting IncorrectParamType {{ integer, Integer, Boolean }}, got {:?}",
                err
            ),
        }
        match req.boolean_param("nosuchparam").unwrap_err() {
            RequestError::MissingParam { name } if name == "nosuchparam" => {}
            err => panic!("expecting MissingParam {{ nosuchparam }}, got {:?}", err),
        };

        assert_eq!(*req.tagged_base64_param("tagged_base64").unwrap(), tb64);
        match req.tagged_base64_param("integer").unwrap_err() {
            RequestError::IncorrectParamType {
                name,
                param_type,
                expected,
            } if name == "integer"
                && param_type == RequestParamType::Integer
                && expected == "TaggedBase64" => {}
            err => panic!(
                "expecting IncorrectParamType {{ integer, Integer, TaggedBase64 }}, got {:?}",
                err
            ),
        }
        match req.tagged_base64_param("nosuchparam").unwrap_err() {
            RequestError::MissingParam { name } if name == "nosuchparam" => {}
            err => panic!("expecting MissingParam {{ nosuchparam }}, got {:?}", err),
        };

        assert_eq!(req.blob_param::<_, Blob>("blob").unwrap(), blob);
        match req.tagged_base64_param("integer").unwrap_err() {
            RequestError::IncorrectParamType {
                name,
                param_type,
                expected,
            } if name == "integer"
                && param_type == RequestParamType::Integer
                && expected == "TaggedBase64" => {}
            err => panic!(
                "expecting IncorrectParamType {{ integer, Integer, TaggedBase64 }}, got {:?}",
                err
            ),
        }
        match req.tagged_base64_param("nosuchparam").unwrap_err() {
            RequestError::MissingParam { name } if name == "nosuchparam" => {}
            err => panic!("expecting MissingParam {{ nosuchparam }}, got {:?}", err),
        };

        // Check typed optional params: correct type, incorrect type, and missing cases.
        assert_eq!(req.opt_string_param("string").unwrap().unwrap(), "hello");
        match req.opt_string_param("integer").unwrap_err() {
            RequestError::IncorrectParamType {
                name,
                param_type,
                expected,
            } if name == "integer"
                && param_type == RequestParamType::Integer
                && expected == "String" => {}
            err => panic!(
                "expecting IncorrectParamType {{ integer, Integer, String }}, got {:?}",
                err
            ),
        }
        assert_eq!(req.opt_string_param("nosuchparam").unwrap(), None);

        assert_eq!(
            req.opt_integer_param::<_, usize>("integer")
                .unwrap()
                .unwrap(),
            42
        );
        match req.opt_integer_param::<_, usize>("string").unwrap_err() {
            RequestError::IncorrectParamType {
                name,
                param_type,
                expected,
            } if name == "string"
                && param_type == RequestParamType::Literal
                && expected == "Integer" => {}
            err => panic!(
                "expecting IncorrectParamType {{ integer, Literal, Integer }}, got {:?}",
                err
            ),
        }
        assert_eq!(
            req.opt_integer_param::<_, usize>("nosuchparam").unwrap(),
            None
        );

        assert_eq!(req.opt_boolean_param("boolean").unwrap().unwrap(), true);
        match req.opt_boolean_param("integer").unwrap_err() {
            RequestError::IncorrectParamType {
                name,
                param_type,
                expected,
            } if name == "integer"
                && param_type == RequestParamType::Integer
                && expected == "Boolean" => {}
            err => panic!(
                "expecting IncorrectParamType {{ integer, Integer, Boolean }}, got {:?}",
                err
            ),
        }
        assert_eq!(req.opt_boolean_param("nosuchparam").unwrap(), None);

        assert_eq!(
            *req.opt_tagged_base64_param("tagged_base64")
                .unwrap()
                .unwrap(),
            tb64
        );
        match req.opt_tagged_base64_param("integer").unwrap_err() {
            RequestError::IncorrectParamType {
                name,
                param_type,
                expected,
            } if name == "integer"
                && param_type == RequestParamType::Integer
                && expected == "TaggedBase64" => {}
            err => panic!(
                "expecting IncorrectParamType {{ integer, Integer, TaggedBase64 }}, got {:?}",
                err
            ),
        }
        assert_eq!(req.opt_tagged_base64_param("nosuchparam").unwrap(), None);

        assert_eq!(
            req.opt_blob_param::<_, Blob>("blob").unwrap().unwrap(),
            blob
        );
        match req.opt_blob_param::<_, Blob>("integer").unwrap_err() {
            RequestError::IncorrectParamType {
                name,
                param_type,
                expected,
            } if name == "integer"
                && param_type == RequestParamType::Integer
                && expected == "TaggedBase64" => {}
            err => panic!(
                "expecting IncorrectParamType {{ integer, Integer, TaggedBase64 }}, got {:?}",
                err
            ),
        }
        assert_eq!(req.opt_blob_param::<_, Blob>("nosuchparam").unwrap(), None);
    }
}
