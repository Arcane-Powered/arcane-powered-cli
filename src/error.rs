use thiserror::Error;

#[derive(Debug, Error)]
pub enum CliError {
    #[error("{message}")]
    Api {
        message: String,
        status: Option<u16>,
        code: Option<String>,
    },

    #[error("{0}")]
    Message(String),

    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("HTTP error: {0}")]
    Http(#[from] reqwest::Error),
}

impl CliError {
    pub fn msg(s: impl Into<String>) -> Self {
        Self::Message(s.into())
    }

    pub fn api(message: impl Into<String>, status: Option<u16>, code: Option<String>) -> Self {
        Self::Api {
            message: message.into(),
            status,
            code,
        }
    }

    pub fn status(&self) -> Option<u16> {
        match self {
            Self::Api { status, .. } => *status,
            _ => None,
        }
    }
}

pub type Result<T> = std::result::Result<T, CliError>;
