use std::fmt;

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// A JSON-RPC request id.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Id {
    Num(i64),
    Str(String),
}

impl fmt::Display for Id {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Id::Num(n) => write!(f, "{n}"),
            Id::Str(s) => f.write_str(s),
        }
    }
}

/// The literal `"2.0"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Version;

impl Serialize for Version {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str("2.0")
    }
}

impl<'de> Deserialize<'de> for Version {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let v = String::deserialize(d)?;
        if v == "2.0" {
            Ok(Version)
        } else {
            Err(serde::de::Error::custom(format!(
                "unsupported jsonrpc version {v:?}"
            )))
        }
    }
}

/// A request, or a notification when `id` is absent.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Request {
    pub jsonrpc: Version,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<Id>,
    pub method: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub params: Option<Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Response {
    pub jsonrpc: Version,
    /// `None` only when answering a request whose id could not be read.
    pub id: Option<Id>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<RpcError>,
}

impl Response {
    pub fn ok(id: Id, result: Value) -> Self {
        Self {
            jsonrpc: Version,
            id: Some(id),
            result: Some(result),
            error: None,
        }
    }

    pub fn err(id: Option<Id>, error: RpcError) -> Self {
        Self {
            jsonrpc: Version,
            id,
            result: None,
            error: Some(error),
        }
    }
}

/// Any message on the wire.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Message {
    Request(Request),
    Response(Response),
}

impl Message {
    pub fn notification(method: impl Into<String>, params: Value) -> Self {
        Message::Request(Request {
            jsonrpc: Version,
            id: None,
            method: method.into(),
            params: Some(params),
        })
    }
}

/// A JSON-RPC error object.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, thiserror::Error)]
#[error("{message} (code {code})")]
pub struct RpcError {
    pub code: i64,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

/// Standard and Glasshouse-specific error codes.
pub mod codes {
    pub const PARSE_ERROR: i64 = -32700;
    pub const INVALID_REQUEST: i64 = -32600;
    pub const METHOD_NOT_FOUND: i64 = -32601;
    pub const INVALID_PARAMS: i64 = -32602;
    pub const INTERNAL_ERROR: i64 = -32603;
    /// The caller may not do this.
    pub const PERMISSION_DENIED: i64 = -32001;
    /// The named task, approval or snapshot does not exist (for this caller).
    pub const NOT_FOUND: i64 = -32004;
    /// The request conflicts with current state, such as approving twice.
    pub const CONFLICT: i64 = -32009;
}

impl RpcError {
    pub fn new(code: i64, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            data: None,
        }
    }

    pub fn method_not_found(method: &str) -> Self {
        Self::new(
            codes::METHOD_NOT_FOUND,
            format!("unknown method {method:?}"),
        )
    }

    pub fn invalid_params(detail: impl fmt::Display) -> Self {
        Self::new(codes::INVALID_PARAMS, format!("invalid params: {detail}"))
    }

    pub fn internal(detail: impl fmt::Display) -> Self {
        Self::new(codes::INTERNAL_ERROR, detail.to_string())
    }

    pub fn permission_denied(detail: impl Into<String>) -> Self {
        Self::new(codes::PERMISSION_DENIED, detail)
    }

    pub fn not_found(detail: impl Into<String>) -> Self {
        Self::new(codes::NOT_FOUND, detail)
    }

    pub fn conflict(detail: impl Into<String>) -> Self {
        Self::new(codes::CONFLICT, detail)
    }
}

/// Deserializes request params, mapping failures to `INVALID_PARAMS`.
/// Absent params read as JSON `null`, which unit-like param types accept.
pub fn parse_params<T: serde::de::DeserializeOwned>(params: Value) -> Result<T, RpcError> {
    serde_json::from_value(params).map_err(RpcError::invalid_params)
}

/// Serializes a handler result.
pub fn to_result<T: Serialize>(value: &T) -> Result<Value, RpcError> {
    serde_json::to_value(value).map_err(RpcError::internal)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn distinguishes_requests_notifications_and_responses() {
        let req: Message =
            serde_json::from_value(json!({"jsonrpc":"2.0","id":1,"method":"m","params":{}}))
                .unwrap();
        assert!(matches!(
            req,
            Message::Request(Request {
                id: Some(Id::Num(1)),
                ..
            })
        ));

        let note: Message = serde_json::from_value(json!({"jsonrpc":"2.0","method":"n"})).unwrap();
        assert!(matches!(note, Message::Request(Request { id: None, .. })));

        let resp: Message =
            serde_json::from_value(json!({"jsonrpc":"2.0","id":"a","result":true})).unwrap();
        assert!(matches!(
            resp,
            Message::Response(Response {
                id: Some(Id::Str(_)),
                ..
            })
        ));
    }

    #[test]
    fn rejects_other_versions() {
        let bad = serde_json::from_value::<Message>(json!({"jsonrpc":"1.0","id":1,"method":"m"}));
        assert!(bad.is_err());
    }
}
