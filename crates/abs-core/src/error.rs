#[derive(Debug, thiserror::Error)]
pub enum CoreError {
    #[error("storage error: {0}")]
    Storage(#[from] abs_storage::StorageError),

    #[error("network error: {0}")]
    Network(#[from] reqwest::Error),

    #[error("authentication failed: {0}")]
    AuthFailed(String),

    #[error("server returned an unexpected response: {0}")]
    UnexpectedResponse(String),
}

pub type Result<T> = std::result::Result<T, CoreError>;
