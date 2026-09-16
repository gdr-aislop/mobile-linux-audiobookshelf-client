#[derive(Debug, thiserror::Error)]
pub enum PlayerError {
    #[error("gstreamer initialization failed: {0}")]
    InitFailed(#[from] gstreamer::glib::Error),

    #[error("failed to build the playback pipeline: {0}")]
    PipelineBuild(#[from] gstreamer::glib::BoolError),

    #[error("failed to change pipeline state: {0}")]
    StateChange(#[from] gstreamer::StateChangeError),

    #[error("seek failed")]
    SeekFailed,

    #[error("no source loaded")]
    NoSourceLoaded,
}

pub type Result<T> = std::result::Result<T, PlayerError>;
