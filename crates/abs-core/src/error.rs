#[derive(Debug, thiserror::Error)]
pub enum CoreError {
    #[error("storage error: {0}")]
    Storage(#[from] abs_storage::StorageError),

    #[error("login failed: {0}")]
    Login(#[from] abs_api::LoginError),

    #[error("server returned an unexpected response: {0}")]
    UnexpectedResponse(String),
}

pub type Result<T> = std::result::Result<T, CoreError>;
