use std::io;

use thiserror::Error;

pub type Result<T> = std::result::Result<T, XferError>;

#[derive(Debug, Error)]
pub enum XferError {
    #[error("I/O error: {0}")]
    Io(#[from] io::Error),

    #[error("protocol error: {0}")]
    Protocol(String),

    #[error("security error: {0}")]
    Security(String),

    #[error("invalid input: {0}")]
    InvalidInput(String),

    #[error("transfer rejected: {0}")]
    Rejected(String),

    #[error("transfer cancelled")]
    Cancelled,

    #[error("serialization error: {0}")]
    Serialization(String),

    #[error("configuration error: {0}")]
    Configuration(String),
}

/// Maps the shared release crate's errors variant by variant rather than
/// through `to_string`, so a failure surfaces with the same wording it had
/// when this crate owned the code.
impl From<rust_cli_release::Error> for XferError {
    fn from(error: rust_cli_release::Error) -> Self {
        match error {
            rust_cli_release::Error::Io(error) => Self::Io(error),
            rust_cli_release::Error::Configuration(message) => Self::Configuration(message),
            rust_cli_release::Error::InvalidInput(message) => Self::InvalidInput(message),
            rust_cli_release::Error::Security(message) => Self::Security(message),
        }
    }
}

impl XferError {
    pub fn protocol(message: impl Into<String>) -> Self {
        Self::Protocol(message.into())
    }

    pub fn security(message: impl Into<String>) -> Self {
        Self::Security(message.into())
    }

    pub fn invalid_input(message: impl Into<String>) -> Self {
        Self::InvalidInput(message.into())
    }
}
