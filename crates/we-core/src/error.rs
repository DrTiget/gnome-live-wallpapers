use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("I/O error at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("invalid JSON in {path}: {source}")]
    Json {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
    #[error("project.json root is not an object")]
    NotAnObject,
    #[error("unknown property `{0}`")]
    UnknownProperty(String),
    #[error("invalid value for property `{key}`: {reason}")]
    InvalidValue { key: String, reason: String },
}

pub type Result<T> = std::result::Result<T, Error>;
