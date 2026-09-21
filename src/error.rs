use std::io;
use std::path::PathBuf;
use std::string::FromUtf8Error;

use lpcli::error::LpError;
use reqwest::StatusCode;
use thiserror::Error;
use url::ParseError;

#[derive(Debug, Error)]
pub enum Error {
    #[error("cannot run {program}: {source}")]
    Command { program: String, source: io::Error },

    #[error("cannot complete {program}: {reason}")]
    CommandFailed { program: String, reason: String },

    #[error("cannot determine the home directory")]
    HomeDirectory,

    #[error("cannot access Launchpad: {source}")]
    Launchpad { source: LpError },

    #[error("cannot access {url}: HTTP {status}")]
    HttpStatus { url: String, status: StatusCode },

    #[error("cannot read {path}: {source}")]
    Io { path: PathBuf, source: io::Error },

    #[error("invalid request: {reason}")]
    InvalidRequest { reason: String },

    #[error("cannot decode command output: {source}")]
    OutputEncoding { source: FromUtf8Error },

    #[error("cannot parse URL {url}: {source}")]
    Url { url: String, source: ParseError },

    #[error("cannot decode URL path: {reason}")]
    UrlEncoding { reason: String },

    #[error("cannot fetch {url}: {source}")]
    Web { url: String, source: reqwest::Error },
}

impl Error {
    pub fn invalid(reason: impl Into<String>) -> Self {
        Self::InvalidRequest {
            reason: reason.into(),
        }
    }
}

impl From<LpError> for Error {
    fn from(source: LpError) -> Self {
        Self::Launchpad { source }
    }
}
