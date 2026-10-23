use std::io;
use std::path::{Path, PathBuf};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{}: {source}", path.display())]
    Path {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error("invalid archive: {0}")]
    Format(String),
    #[error("unsupported: {0}")]
    Unsupported(String),
    #[error("{0}")]
    Msg(String),
    #[error("interrupted")]
    Cancelled,
}

impl Error {
    pub fn path(path: impl Into<PathBuf>, source: io::Error) -> Self {
        Error::Path { path: path.into(), source }
    }

    pub fn format(msg: impl Into<String>) -> Self {
        Error::Format(msg.into())
    }

    pub fn msg(msg: impl Into<String>) -> Self {
        Error::Msg(msg.into())
    }
}

impl From<Error> for io::Error {
    fn from(e: Error) -> Self {
        match e {
            Error::Io(e) => e,
            Error::Path { source, .. } => source,
            other => io::Error::other(other.to_string()),
        }
    }
}

pub type Result<T, E = Error> = std::result::Result<T, E>;

pub trait IoResultExt<T> {
    /// Attaches a path to an I/O error.
    fn at(self, path: &Path) -> Result<T>;
}

impl<T> IoResultExt<T> for io::Result<T> {
    fn at(self, path: &Path) -> Result<T> {
        self.map_err(|e| Error::path(path, e))
    }
}
