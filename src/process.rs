//! Linux process identity, port allocation, signals, and readiness.
#![allow(dead_code)]

use std::ffi::OsString;
use std::fmt;
use std::fs;
use std::io;
use std::net::{Ipv4Addr, SocketAddrV4, TcpListener};
use std::os::unix::ffi::{OsStrExt, OsStringExt};

use crate::state::Lease;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Liveness {
    Alive,
    Dead,
    Indeterminate,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ProcIdentity {
    pgid: i32,
    start_time_ticks: u64,
}

#[derive(Debug)]
pub(crate) enum Error {
    InvalidPort,
    PortInUse(u16),
    Bind(io::Error),
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidPort => write!(formatter, "application port must be between 1 and 65535"),
            Self::PortInUse(port) => write!(
                formatter,
                "requested application port {port} is already in use"
            ),
            Self::Bind(error) => write!(formatter, "cannot reserve a loopback port: {error}"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Bind(error) => Some(error),
            _ => None,
        }
    }
}

pub(crate) struct PortReservation {
    listener: TcpListener,
    pub(crate) port: u16,
    pub(crate) warning: Option<String>,
}

impl PortReservation {
    pub(crate) fn release(self) -> u16 {
        let port = self.port;
        drop(self.listener);
        port
    }
}

pub(crate) fn reserve_port(preferred: Option<u16>, strict: bool) -> Result<PortReservation, Error> {
    if preferred == Some(0) {
        return Err(Error::InvalidPort);
    }
    if let Some(port) = preferred {
        match bind(port) {
            Ok(listener) => {
                return Ok(PortReservation {
                    listener,
                    port,
                    warning: None,
                });
            }
            Err(error) if error.kind() == io::ErrorKind::AddrInUse && strict => {
                return Err(Error::PortInUse(port));
            }
            Err(error) if error.kind() == io::ErrorKind::AddrInUse => {
                let listener = bind(0).map_err(Error::Bind)?;
                let fallback = listener.local_addr().map_err(Error::Bind)?.port();
                return Ok(PortReservation {
                    listener,
                    port: fallback,
                    warning: Some(format!(
                        "requested port {port} is occupied; using {fallback}"
                    )),
                });
            }
            Err(error) => return Err(Error::Bind(error)),
        }
    }
    let listener = bind(0).map_err(Error::Bind)?;
    let port = listener.local_addr().map_err(Error::Bind)?.port();
    Ok(PortReservation {
        listener,
        port,
        warning: None,
    })
}

fn bind(port: u16) -> io::Result<TcpListener> {
    TcpListener::bind(SocketAddrV4::new(Ipv4Addr::LOCALHOST, port))
}

pub(crate) fn substitute_port(arguments: &[OsString], port: u16) -> Vec<OsString> {
    let replacement = port.to_string();
    arguments
        .iter()
        .map(|argument| {
            OsString::from_vec(replace_bytes(
                argument.as_bytes(),
                b"{port}",
                replacement.as_bytes(),
            ))
        })
        .collect()
}

fn replace_bytes(input: &[u8], needle: &[u8], replacement: &[u8]) -> Vec<u8> {
    let mut output = Vec::with_capacity(input.len());
    let mut rest = input;
    while let Some(position) = rest
        .windows(needle.len())
        .position(|window| window == needle)
    {
        output.extend_from_slice(&rest[..position]);
        output.extend_from_slice(replacement);
        rest = &rest[position + needle.len()..];
    }
    output.extend_from_slice(rest);
    output
}

pub(crate) fn child_environment(port: u16, hostname: &str, tls: bool) -> [(OsString, OsString); 3] {
    [
        (OsString::from("PORT"), OsString::from(port.to_string())),
        (OsString::from("HOST"), OsString::from("127.0.0.1")),
        (
            OsString::from("NOOK_URL"),
            OsString::from(format!(
                "{}://{hostname}",
                if tls { "https" } else { "http" }
            )),
        ),
    ]
}

pub(crate) fn lease_liveness(lease: &Lease) -> Liveness {
    match fs::read_to_string(format!("/proc/{}/stat", lease.pid)) {
        Ok(stat) => match parse_stat(&stat) {
            Some(identity)
                if identity.pgid == lease.pgid
                    && identity.start_time_ticks == lease.process_start_time_ticks =>
            {
                Liveness::Alive
            }
            Some(_) => Liveness::Dead,
            None => Liveness::Indeterminate,
        },
        Err(error) if error.kind() == io::ErrorKind::NotFound => Liveness::Dead,
        Err(_) => Liveness::Indeterminate,
    }
}

fn parse_stat(stat: &str) -> Option<ProcIdentity> {
    let command_end = stat.rfind(')')?;
    let fields: Vec<_> = stat.get(command_end + 1..)?.split_whitespace().collect();
    Some(ProcIdentity {
        pgid: fields.get(2)?.parse().ok()?,
        start_time_ticks: fields.get(19)?.parse().ok()?,
    })
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;
    use std::net::{Ipv4Addr, SocketAddrV4, TcpListener};
    use std::os::unix::ffi::{OsStrExt, OsStringExt};

    use super::{
        Error, ProcIdentity, child_environment, parse_stat, reserve_port, substitute_port,
    };

    #[test]
    fn parses_pgid_and_start_time_even_when_comm_contains_spaces_and_parentheses() {
        let stat =
            "42 (worker (test) name) S 1 77 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 98765 21";
        assert_eq!(
            parse_stat(stat),
            Some(ProcIdentity {
                pgid: 77,
                start_time_ticks: 98765
            })
        );
    }

    #[test]
    fn malformed_or_truncated_proc_data_is_indeterminate() {
        assert_eq!(parse_stat("42 (worker) S 1"), None);
        assert_eq!(parse_stat("not proc stat"), None);
    }

    #[test]
    fn current_process_identity_is_readable_and_stable() {
        let stat = std::fs::read_to_string(format!("/proc/{}/stat", std::process::id())).unwrap();
        let first = parse_stat(&stat).unwrap();
        let second = parse_stat(
            &std::fs::read_to_string(format!("/proc/{}/stat", std::process::id())).unwrap(),
        )
        .unwrap();
        assert_eq!(first, second);
        assert!(first.start_time_ticks > 0);
    }

    #[test]
    fn ephemeral_reservation_binds_loopback_and_holds_the_port() {
        let reservation = reserve_port(None, false).unwrap();
        let port = reservation.port;
        assert!(TcpListener::bind(SocketAddrV4::new(Ipv4Addr::LOCALHOST, port)).is_err());
        reservation.release();
        assert!(TcpListener::bind(SocketAddrV4::new(Ipv4Addr::LOCALHOST, port)).is_ok());
    }

    #[test]
    fn occupied_preferred_port_falls_back_or_fails_strictly() {
        let occupied = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let port = occupied.local_addr().unwrap().port();
        let fallback = reserve_port(Some(port), false).unwrap();
        assert_ne!(fallback.port, port);
        assert!(fallback.warning.unwrap().contains("occupied"));
        assert!(
            matches!(reserve_port(Some(port), true), Err(Error::PortInUse(value)) if value == port)
        );
    }

    #[test]
    fn substitution_is_literal_repeated_and_non_utf8_safe() {
        let raw = OsString::from_vec(vec![0x80, b'-', b'{', b'p', b'o', b'r', b't', b'}']);
        let replaced = substitute_port(&[OsString::from("--port={port}:{port}"), raw], 4321);
        assert_eq!(replaced[0], "--port=4321:4321");
        assert_eq!(
            replaced[1].as_bytes(),
            &[0x80, b'-', b'4', b'3', b'2', b'1']
        );
    }

    #[test]
    fn child_environment_replaces_required_values() {
        let environment = child_environment(3000, "api.localhost", true);
        assert_eq!(
            environment[0],
            (OsString::from("PORT"), OsString::from("3000"))
        );
        assert_eq!(
            environment[1],
            (OsString::from("HOST"), OsString::from("127.0.0.1"))
        );
        assert_eq!(
            environment[2],
            (
                OsString::from("NOOK_URL"),
                OsString::from("https://api.localhost")
            )
        );
    }
}
