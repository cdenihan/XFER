use std::io;

use std::fmt;

pub type Result<T> = std::result::Result<T, XferError>;

#[derive(Debug)]
pub enum XferError {
    Io(io::Error),

    Protocol(String),

    Security(String),

    InvalidInput(String),

    Rejected(String),

    Cancelled,

    Serialization(String),

    Configuration(String),
}

impl fmt::Display for XferError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(f, "I/O error: {error}"),
            Self::Protocol(message) => write!(f, "protocol error: {message}"),
            Self::Security(message) => write!(f, "security error: {message}"),
            Self::InvalidInput(message) => write!(f, "invalid input: {message}"),
            Self::Rejected(message) => write!(f, "transfer rejected: {message}"),
            Self::Cancelled => f.write_str("transfer cancelled"),
            Self::Serialization(message) => write!(f, "serialization error: {message}"),
            Self::Configuration(message) => write!(f, "configuration error: {message}"),
        }
    }
}
impl std::error::Error for XferError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        if let Self::Io(error) = self {
            Some(error)
        } else {
            None
        }
    }
}
impl From<io::Error> for XferError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}
impl From<serde_json::Error> for XferError {
    fn from(error: serde_json::Error) -> Self {
        Self::Serialization(error.to_string())
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
