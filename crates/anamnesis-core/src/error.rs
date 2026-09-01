//! Error type shared by the core primitives.

use std::path::PathBuf;

/// Errors produced by core identity, scope, config, and validation routines.
#[derive(Debug, thiserror::Error)]
pub enum CoreError {
    /// The data directory could not be located or created.
    #[error("data directory could not be resolved: {reason}")]
    DataDir {
        /// Human-readable explanation of what went wrong.
        reason: String,
    },

    /// Scope resolution failed for the given working directory.
    #[error("scope could not be resolved from {cwd}: {reason}")]
    Scope {
        /// Directory the resolution started from.
        cwd: PathBuf,
        /// Human-readable explanation of what went wrong.
        reason: String,
    },

    /// A workspace or project name failed validation.
    #[error("invalid {kind} name {value:?}: {reason}")]
    InvalidName {
        /// Which kind of name was rejected (`workspace` or `project`).
        kind: &'static str,
        /// The offending value.
        value: String,
        /// Why it was rejected.
        reason: &'static str,
    },

    /// A wiki page path failed validation.
    #[error("invalid page path {path:?}: {reason}")]
    InvalidPagePath {
        /// The offending path.
        path: String,
        /// Why it was rejected.
        reason: &'static str,
    },

    /// An observation body exceeded its byte budget.
    #[error("body of {actual} bytes exceeds the {limit} byte limit")]
    BodyTooLarge {
        /// Size of the supplied body.
        actual: usize,
        /// Maximum permitted size.
        limit: usize,
    },

    /// Configuration could not be loaded or deserialized.
    ///
    /// Boxed: `figment::Error` is large enough that carrying it inline would
    /// widen every `Result` in the crate.
    #[error("configuration error: {0}")]
    Config(Box<figment::Error>),

    /// A numeric setting in the marker file is outside its permitted range.
    ///
    /// Separate from [`Self::Config`] because a value that parses as a number
    /// and then means something absurd — a half-life of zero, a negative
    /// threshold — never reaches the deserializer at all.
    #[error("invalid setting {key} = {value}: {reason}")]
    InvalidSetting {
        /// Dotted key path, as it appears in the marker file.
        key: &'static str,
        /// The offending value.
        value: f64,
        /// Why it was rejected.
        reason: &'static str,
    },

    /// A key in the marker file that belongs to no table this build knows.
    ///
    /// Separate from [`Self::Config`] because it is the *shape* that decides:
    /// an unknown table is a feature from a newer anamnesis and is skipped
    /// with a warning, while a bare key outside every table is what a typo
    /// looks like — `workspace = "x"` written above `[scope]` rather than
    /// inside it — and refusing that is what keeps memory from going
    /// somewhere nobody meant.
    #[error("unknown setting {key:?} outside any table in {origin}")]
    UnknownSetting {
        /// The offending key, as it appears in the file.
        key: String,
        /// Where it was read from.
        origin: String,
    },

    /// A git operation failed while inspecting the repository.
    #[error("git error: {0}")]
    Git(Box<git2::Error>),

    /// A filesystem operation failed.
    #[error("io error at {path}: {source}")]
    Io {
        /// Path the operation was attempted on.
        path: PathBuf,
        /// Underlying cause.
        #[source]
        source: std::io::Error,
    },
}

impl From<figment::Error> for CoreError {
    fn from(source: figment::Error) -> Self {
        Self::Config(Box::new(source))
    }
}

impl From<git2::Error> for CoreError {
    fn from(source: git2::Error) -> Self {
        Self::Git(Box::new(source))
    }
}

impl CoreError {
    /// Build an [`CoreError::Io`] carrying the path that failed.
    pub fn io(path: impl Into<PathBuf>, source: std::io::Error) -> Self {
        Self::Io {
            path: path.into(),
            source,
        }
    }
}

/// Convenience alias for results produced by this crate.
pub type Result<T> = std::result::Result<T, CoreError>;
