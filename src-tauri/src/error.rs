use serde::Serialize;

/// Everything that can go wrong, in a shape the UI can act on.
///
/// `kind` is for the UI to branch on, `message` is what a person reads. The
/// messages are deliberately specific: "request failed" tells somebody setting
/// this up nothing about whether they typed the URL wrong, pasted a dead token,
/// or pointed it at a Fireshare too old to have the endpoint at all.
#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error("{0}")]
    BadUrl(String),

    #[error("{0}")]
    Unreachable(String),

    #[error("{0}")]
    NotFireshare(String),

    #[error("{0}")]
    TokenRejected(String),

    #[error("{0}")]
    Throttled(String),

    #[error("{0}")]
    Server(String),

    #[error("{0}")]
    Keychain(String),

    #[error("{0}")]
    Storage(String),

    #[error("{0}")]
    NotConnected(String),
}

impl AppError {
    pub fn kind(&self) -> &'static str {
        match self {
            AppError::BadUrl(_) => "bad_url",
            AppError::Unreachable(_) => "unreachable",
            AppError::NotFireshare(_) => "not_fireshare",
            AppError::TokenRejected(_) => "token_rejected",
            AppError::Throttled(_) => "throttled",
            AppError::Server(_) => "server",
            AppError::Keychain(_) => "keychain",
            AppError::Storage(_) => "storage",
            AppError::NotConnected(_) => "not_connected",
        }
    }
}

impl Serialize for AppError {
    // Spelled out because the `Result` alias below shadows the std one here.
    fn serialize<S: serde::Serializer>(
        &self,
        serializer: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        let mut s = serializer.serialize_struct("AppError", 2)?;
        s.serialize_field("kind", self.kind())?;
        s.serialize_field("message", &self.to_string())?;
        s.end()
    }
}

pub type Result<T> = std::result::Result<T, AppError>;
