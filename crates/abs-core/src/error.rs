#[derive(Debug, thiserror::Error)]
pub enum CoreError {
    #[error("storage error: {0}")]
    Storage(#[from] abs_storage::StorageError),

    #[error("login failed: {0}")]
    Login(#[from] abs_api::LoginError),

    #[error("the server rejected this session's credentials — sign in again")]
    Auth,

    #[error("server returned an unexpected response: {0}")]
    UnexpectedResponse(String),

    /// The app's offline mode is on: nothing may reach the server until it's switched off.
    #[error("offline mode is on")]
    Offline,
}

pub type Result<T> = std::result::Result<T, CoreError>;
