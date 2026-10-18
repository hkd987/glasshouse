use glass_rpc::RpcError;

/// Why a control request failed. Maps one to one onto JSON-RPC error codes.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RuntimeError {
    #[error("{0}")]
    NotFound(String),
    #[error("{0}")]
    PermissionDenied(String),
    #[error("{0}")]
    Invalid(String),
    #[error("{0}")]
    Conflict(String),
    #[error("{0}")]
    Internal(String),
}

impl From<RuntimeError> for RpcError {
    fn from(e: RuntimeError) -> Self {
        match e {
            RuntimeError::NotFound(m) => RpcError::not_found(m),
            RuntimeError::PermissionDenied(m) => RpcError::permission_denied(m),
            RuntimeError::Invalid(m) => RpcError::invalid_params(m),
            RuntimeError::Conflict(m) => RpcError::conflict(m),
            RuntimeError::Internal(m) => RpcError::internal(m),
        }
    }
}

pub type Result<T, E = RuntimeError> = std::result::Result<T, E>;
