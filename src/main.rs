#![forbid(unsafe_code)]

mod caddy;
mod cli;
mod config;
mod process;
mod reconcile;
mod state;

use std::fmt;
use std::io;
use std::process::ExitCode;

pub(crate) type Result<T> = std::result::Result<T, Error>;

/// Error returned by the Nook application boundary.
///
/// Concrete variants are added only when their corresponding operations are
/// implemented. Internal modules use this common type instead of exposing an
/// error API outside the binary crate.
#[derive(Debug)]
pub(crate) enum Error {
    Io(io::Error),
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(formatter, "I/O error: {error}"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
        }
    }
}

impl From<io::Error> for Error {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

fn main() -> ExitCode {
    match cli::run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("nook: {error}");
            ExitCode::FAILURE
        }
    }
}
