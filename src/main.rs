#![deny(unsafe_code)]

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
    Caddy(caddy::Error),
    Cli(clap::Error),
    Config(config::Error),
    State(state::Error),
    Alias(reconcile::AliasError),
    Run(process::RunError),
    Stop(process::StopError),
    Io(io::Error),
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Caddy(error) => error.fmt(formatter),
            Self::Cli(error) => error.fmt(formatter),
            Self::Config(error) => error.fmt(formatter),
            Self::State(error) => error.fmt(formatter),
            Self::Alias(error) => error.fmt(formatter),
            Self::Run(error) => error.fmt(formatter),
            Self::Stop(error) => error.fmt(formatter),
            Self::Io(error) => write!(formatter, "I/O error: {error}"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Caddy(error) => Some(error),
            Self::Cli(error) => Some(error),
            Self::Config(error) => Some(error),
            Self::State(error) => Some(error),
            Self::Alias(error) => Some(error),
            Self::Run(error) => Some(error),
            Self::Stop(error) => Some(error),
            Self::Io(error) => Some(error),
        }
    }
}

impl Error {
    fn exit_code(&self) -> ExitCode {
        match self {
            Self::Caddy(_) => ExitCode::FAILURE,
            Self::Cli(error) => {
                u8::try_from(error.exit_code()).map_or(ExitCode::FAILURE, ExitCode::from)
            }
            Self::Config(_) => ExitCode::FAILURE,
            Self::State(_) | Self::Alias(_) | Self::Run(_) | Self::Stop(_) => ExitCode::FAILURE,
            Self::Io(_) => ExitCode::FAILURE,
        }
    }
}

impl From<caddy::Error> for Error {
    fn from(error: caddy::Error) -> Self {
        Self::Caddy(error)
    }
}

impl From<clap::Error> for Error {
    fn from(error: clap::Error) -> Self {
        Self::Cli(error)
    }
}

impl From<config::Error> for Error {
    fn from(error: config::Error) -> Self {
        Self::Config(error)
    }
}

impl From<io::Error> for Error {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<state::Error> for Error {
    fn from(error: state::Error) -> Self {
        Self::State(error)
    }
}

impl From<reconcile::AliasError> for Error {
    fn from(error: reconcile::AliasError) -> Self {
        Self::Alias(error)
    }
}

impl From<process::RunError> for Error {
    fn from(error: process::RunError) -> Self {
        Self::Run(error)
    }
}

impl From<process::StopError> for Error {
    fn from(error: process::StopError) -> Self {
        Self::Stop(error)
    }
}

fn main() -> ExitCode {
    match cli::run() {
        Ok(code) => u8::try_from(code).map_or(ExitCode::FAILURE, ExitCode::from),
        Err(error) => {
            eprintln!("error: {error}");
            error.exit_code()
        }
    }
}
