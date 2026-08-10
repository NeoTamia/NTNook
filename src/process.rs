//! Linux process identity, port allocation, signals, and readiness.
#![allow(dead_code)]

use std::ffi::OsString;
use std::fmt;
use std::fs;
use std::io;
use std::net::{Ipv4Addr, SocketAddrV4, TcpListener};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::process::{Child, Command, Stdio};
use std::thread;

use signal_hook::consts::{SIGINT, SIGTERM};
use signal_hook::iterator::Signals;

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
    EmptyCommand,
    Spawn(io::Error),
    ProcessIdentity(u32),
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
            Self::EmptyCommand => write!(formatter, "child command argv cannot be empty"),
            Self::Spawn(error) => write!(formatter, "cannot launch child process: {error}"),
            Self::ProcessIdentity(pid) => {
                write!(formatter, "cannot read identity of child process {pid}")
            }
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Bind(error) | Self::Spawn(error) => Some(error),
            _ => None,
        }
    }
}

pub(crate) struct ManagedChild {
    child: Child,
    pub(crate) pid: u32,
    pub(crate) pgid: i32,
    pub(crate) start_time_ticks: u64,
}

impl ManagedChild {
    pub(crate) fn signal_group(&self, signal: i32) -> Result<(), Error> {
        send_group_signal(self.pgid, signal).map_err(Error::Spawn)
    }

    pub(crate) fn wait(&mut self) -> Result<i32, Error> {
        let mut signals = Signals::new([SIGINT, SIGTERM]).map_err(Error::Spawn)?;
        let handle = signals.handle();
        let pgid = self.pgid;
        let forwarder = thread::spawn(move || {
            for signal in signals.forever() {
                let _ = send_group_signal(pgid, signal);
            }
        });
        let status = self.child.wait().map_err(Error::Spawn)?;
        handle.close();
        let _ = forwarder.join();
        Ok(status
            .code()
            .unwrap_or_else(|| 128 + status.signal().unwrap_or(1)))
    }
}

pub(crate) fn spawn_child(
    argv: &[OsString],
    environment: &[(OsString, OsString)],
) -> Result<ManagedChild, Error> {
    let (program, arguments) = argv.split_first().ok_or(Error::EmptyCommand)?;
    let mut command = Command::new(program);
    command.args(arguments).envs(environment.iter().cloned());
    command
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());
    configure_linux_child(&mut command);
    let mut child = command.spawn().map_err(Error::Spawn)?;
    let pid = child.id();
    let Some(identity) = read_proc_identity(pid) else {
        let _ = send_group_signal(pid as i32, libc::SIGKILL);
        let _ = child.wait();
        return Err(Error::ProcessIdentity(pid));
    };
    Ok(ManagedChild {
        child,
        pid,
        pgid: identity.pgid,
        start_time_ticks: identity.start_time_ticks,
    })
}

#[allow(unsafe_code)]
fn configure_linux_child(command: &mut Command) {
    // SAFETY: this closure runs after fork and before exec. It only invokes
    // async-signal-safe libc syscalls and constructs errors from errno.
    unsafe {
        command.pre_exec(|| {
            if libc::setpgid(0, 0) == -1 {
                return Err(io::Error::last_os_error());
            }
            if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM) == -1 {
                return Err(io::Error::last_os_error());
            }
            if libc::getppid() == 1 {
                libc::raise(libc::SIGTERM);
            }
            Ok(())
        });
    }
}

#[allow(unsafe_code)]
fn send_group_signal(pgid: i32, signal: i32) -> io::Result<()> {
    // SAFETY: pgid is read from /proc for the spawned child and negating it
    // asks kill(2) to target exactly that process group.
    if unsafe { libc::kill(-pgid, signal) } == -1 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

fn read_proc_identity(pid: u32) -> Option<ProcIdentity> {
    fs::read_to_string(format!("/proc/{pid}/stat"))
        .ok()
        .and_then(|stat| parse_stat(&stat))
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
        Error, ProcIdentity, child_environment, parse_stat, reserve_port, spawn_child,
        substitute_port,
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

    #[test]
    fn child_runs_in_its_own_group_and_returns_its_exit_code() {
        let mut child = spawn_child(
            &[
                OsString::from("/bin/sh"),
                OsString::from("-c"),
                OsString::from("sleep 0.05; exit 7"),
            ],
            &[],
        )
        .unwrap();
        assert_eq!(child.pgid, child.pid as i32);
        assert!(child.start_time_ticks > 0);
        assert_eq!(child.wait().unwrap(), 7);
    }

    #[test]
    fn group_signal_returns_conventional_signal_exit_code() {
        let mut child =
            spawn_child(&[OsString::from("/bin/sleep"), OsString::from("10")], &[]).unwrap();
        child.signal_group(libc::SIGTERM).unwrap();
        assert_eq!(child.wait().unwrap(), 128 + libc::SIGTERM);
    }
}
