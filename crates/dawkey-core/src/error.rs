// SPDX-License-Identifier: AGPL-3.0-or-later

use std::path::PathBuf;

use crate::model::{ClipId, SourceId, TrackId};

#[derive(Debug, thiserror::Error)]
pub enum CoreError {
    #[error("track {0} does not exist")]
    TrackNotFound(TrackId),

    #[error("clip {0} does not exist")]
    ClipNotFound(ClipId),

    #[error("source {0} does not exist")]
    SourceNotFound(SourceId),

    #[error("invalid argument: {0}")]
    InvalidArgument(String),

    #[error("{path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("{path}: not a valid Dawkey project: {reason}")]
    ProjectFormat { path: PathBuf, reason: String },
}

impl CoreError {
    pub(crate) fn invalid(msg: impl Into<String>) -> Self {
        Self::InvalidArgument(msg.into())
    }

    pub(crate) fn io(path: impl Into<PathBuf>, source: std::io::Error) -> Self {
        Self::Io {
            path: path.into(),
            source,
        }
    }
}
