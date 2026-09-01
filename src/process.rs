//! Cross-platform process identity, port allocation, signals, and readiness.
#![allow(dead_code)]

#[cfg(windows)]
use std::env;
use std::ffi::OsString;
use std::fmt;
#[cfg(unix)]
use std::fs;
use std::io;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, TcpListener, TcpStream};
#[cfg(unix)]
use std::os::unix::ffi::{OsStrExt, OsStringExt};
#[cfg(unix)]
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::process::{Child, Command, Stdio};
#[cfg(windows)]
use std::sync::OnceLock;
#[cfg(windows)]
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

#[cfg(unix)]
use signal_hook::consts::{SIGINT, SIGTERM};
#[cfg(unix)]
use signal_hook::iterator::Signals;

use crate::config::ResolvedRunConfig;
use crate::reconcile::{RouteBackend, RouteError, RouteSpec};
use crate::state::Lease;
use crate::state::{LeaseState, PendingOperation, PendingOperationKind, Scheme, Store};
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Liveness {
    Alive,
    Dead,
    Indeterminate,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ProcIdentity {
    state: u8,
    pgid: i32,
    start_time_ticks: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ProcessSignal {
    Interrupt,
    Terminate,
    Kill,
}

struct ForwardedSignals {
    #[cfg(unix)]
    inner: Signals,
}

#[cfg(windows)]
static WINDOWS_INTERRUPTED: AtomicBool = AtomicBool::new(false);
#[cfg(windows)]
static WINDOWS_SIGNAL_HANDLER: OnceLock<Result<(), String>> = OnceLock::new();

impl ForwardedSignals {
    fn new() -> io::Result<Self> {
        #[cfg(unix)]
        {
            Signals::new([SIGINT, SIGTERM]).map(|inner| Self { inner })
        }
        #[cfg(windows)]
        {
            WINDOWS_SIGNAL_HANDLER
                .get_or_init(|| {
                    ctrlc::set_handler(|| WINDOWS_INTERRUPTED.store(true, Ordering::SeqCst))
                        .map_err(|error| error.to_string())
                })
                .as_ref()
                .map_err(|error| io::Error::other(error.clone()))?;
            Ok(Self {})
        }
    }

    fn pending(&mut self) -> Vec<ProcessSignal> {
        #[cfg(unix)]
        {
            self.inner
                .pending()
                .map(|signal| {
                    if signal == SIGINT {
                        ProcessSignal::Interrupt
                    } else {
                        ProcessSignal::Terminate
                    }
                })
                .collect()
        }
        #[cfg(windows)]
        {
            if WINDOWS_INTERRUPTED.swap(false, Ordering::SeqCst) {
                vec![ProcessSignal::Interrupt]
            } else {
                Vec::new()
            }
        }
    }
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
                "requested application port {port} is already in use; choose another port or omit --strict-port"
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
    #[cfg(windows)]
    job: windows_sys::Win32::Foundation::HANDLE,
}

impl ManagedChild {
    pub(crate) fn signal(&self, signal: ProcessSignal) -> Result<(), Error> {
        signal_managed_child(self, signal).map_err(Error::Spawn)
    }

    pub(crate) fn wait(&mut self) -> Result<i32, Error> {
        let mut signals = ForwardedSignals::new().map_err(Error::Spawn)?;
        self.wait_with_signals(&mut signals)
    }

    fn wait_with_signals(&mut self, signals: &mut ForwardedSignals) -> Result<i32, Error> {
        loop {
            for signal in signals.pending() {
                let _ = self.signal(signal);
            }
            if let Some(status) = self.child.try_wait().map_err(Error::Spawn)? {
                #[cfg(unix)]
                return Ok(status
                    .code()
                    .unwrap_or_else(|| 128 + status.signal().unwrap_or(1)));
                #[cfg(windows)]
                return Ok(status.code().unwrap_or(1));
            }
            thread::sleep(Duration::from_millis(20));
        }
    }

    fn has_exited(&mut self) -> Result<bool, Error> {
        self.child
            .try_wait()
            .map(|status| status.is_some())
            .map_err(Error::Spawn)
    }
}

#[derive(Debug)]
pub(crate) enum RunError {
    State(crate::state::Error),
    Route(RouteError),
    Process(Error),
    Conflict(String),
}

impl fmt::Display for RunError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::State(error) => error.fmt(formatter),
            Self::Route(error) => error.fmt(formatter),
            Self::Process(error) => error.fmt(formatter),
            Self::Conflict(hostname) => write!(
                formatter,
                "hostname `{hostname}` is already managed by Nook; use --force to replace it"
            ),
        }
    }
}

impl std::error::Error for RunError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::State(error) => Some(error),
            Self::Route(error) => Some(error),
            Self::Process(error) => Some(error),
            Self::Conflict(_) => None,
        }
    }
}

impl From<crate::state::Error> for RunError {
    fn from(error: crate::state::Error) -> Self {
        Self::State(error)
    }
}
impl From<RouteError> for RunError {
    fn from(error: RouteError) -> Self {
        Self::Route(error)
    }
}
impl From<Error> for RunError {
    fn from(error: Error) -> Self {
        Self::Process(error)
    }
}

pub(crate) struct RunningChild {
    pub(crate) child: ManagedChild,
    pub(crate) lease_id: Uuid,
    pub(crate) hostname: String,
    pub(crate) port: u16,
    pub(crate) warning: Option<String>,
    tls: bool,
    bind_address: IpAddr,
    readiness_warn_after: Duration,
    signals: ForwardedSignals,
}

pub(crate) fn start_run(
    config: &ResolvedRunConfig,
    store: &Store,
    routes: &mut impl RouteBackend,
) -> Result<RunningChild, RunError> {
    start_run_with_hook(config, store, routes, |_| {})
}

fn start_run_with_hook(
    config: &ResolvedRunConfig,
    store: &Store,
    routes: &mut impl RouteBackend,
    after_release: impl FnOnce(u16),
) -> Result<RunningChild, RunError> {
    let signals = ForwardedSignals::new().map_err(Error::Spawn)?;
    let _operations = store.lock_operations()?;
    let conflicts = store.mutate(|registry| {
        let aliases = registry
            .aliases
            .values()
            .filter(|alias| alias.hostname == config.hostname)
            .map(|alias| (alias.id, alias.tls));
        let leases = registry
            .leases
            .values()
            .filter(|lease| lease.hostname == config.hostname)
            .map(|lease| (lease.id, lease.tls));
        Ok(aliases.chain(leases).collect::<Vec<_>>())
    })?;
    if !conflicts.is_empty() && !config.force {
        return Err(RunError::Conflict(config.hostname.clone()));
    }
    let reservation = reserve_port_on(config.bind_address, config.app_port, config.strict_port)?;
    let port = reservation.port;
    let owner_id = Uuid::new_v4();
    let operation_id = Uuid::new_v4();
    let target = format!("http://127.0.0.1:{port}");
    let route = RouteSpec {
        owner_id,
        hostname: config.hostname.clone(),
        target: target.clone(),
        scheme: Scheme::Http,
        tls: config.tls,
        replace_existing: config.force,
        preserve_host: false,
    };
    store.mutate(|registry| {
        registry.pending_operations.push(PendingOperation {
            id: operation_id,
            kind: PendingOperationKind::InstallRoute {
                hostname: route.hostname.clone(),
                target: target.clone(),
                scheme: Scheme::Http,
                owner_id,
                tls: config.tls,
            },
        });
        Ok(())
    })?;
    routes.ensure(&route)?;
    let cleanup_results: Vec<_> = conflicts
        .iter()
        .map(|(id, tls)| {
            (
                *id,
                *tls,
                routes.remove_if_owned(&config.hostname, *id, *tls),
            )
        })
        .collect();
    store.mutate(|registry| {
        let replaced_ids: std::collections::BTreeSet<_> =
            conflicts.iter().map(|(id, _)| *id).collect();
        registry
            .aliases
            .retain(|_, alias| !replaced_ids.contains(&alias.id));
        registry.leases.retain(|id, _| !replaced_ids.contains(id));
        registry.pending_operations.retain(|operation| {
            pending_owner(&operation.kind).is_none_or(|id| !replaced_ids.contains(&id))
        });
        for (owner_id, tls, result) in &cleanup_results {
            if result.is_err() {
                registry.pending_operations.push(PendingOperation {
                    id: Uuid::new_v4(),
                    kind: PendingOperationKind::RemoveRoute {
                        hostname: config.hostname.clone(),
                        owner_id: *owner_id,
                        tls: *tls,
                    },
                });
            }
        }
        replace_operation(
            registry,
            operation_id,
            PendingOperationKind::StartProcess {
                hostname: route.hostname.clone(),
                target: target.clone(),
                scheme: Scheme::Http,
                owner_id,
                tls: config.tls,
            },
        );
        Ok(())
    })?;

    let argv = substitute_port(&config.command, port);
    let environment = child_environment(port, config.bind_address, &config.hostname, config.tls);
    let mut warnings: Vec<String> = reservation.warning.iter().cloned().collect();
    if !conflicts.is_empty() {
        warnings.push(format!(
            "replaced existing Nook route for {}; the previous owner is no longer managed",
            config.hostname
        ));
    }
    for (_, _, result) in &cleanup_results {
        if let Err(error) = result {
            warnings.push(format!("cleanup of the previous route is pending: {error}"));
        }
    }
    let warning = (!warnings.is_empty()).then(|| warnings.join("; "));
    reservation.release();
    after_release(port);
    let child = match spawn_managed_child(&argv, &environment, owner_id) {
        Ok(child) => child,
        Err(error) => {
            let cleanup = routes.remove_if_owned(&config.hostname, owner_id, config.tls);
            store.mutate(|registry| {
                registry
                    .pending_operations
                    .retain(|operation| operation.id != operation_id);
                if cleanup.is_err() {
                    registry.pending_operations.push(PendingOperation {
                        id: Uuid::new_v4(),
                        kind: PendingOperationKind::RemoveRoute {
                            hostname: config.hostname.clone(),
                            owner_id,
                            tls: config.tls,
                        },
                    });
                }
                Ok(())
            })?;
            return Err(error.into());
        }
    };
    store.mutate(|registry| {
        registry
            .pending_operations
            .retain(|operation| operation.id != operation_id);
        registry.leases.insert(
            owner_id,
            Lease {
                id: owner_id,
                hostname: config.hostname.clone(),
                target,
                scheme: Scheme::Http,
                tls: config.tls,
                pid: child.pid,
                pgid: child.pgid,
                process_start_time_ticks: child.start_time_ticks,
                state: LeaseState::Starting,
            },
        );
        Ok(())
    })?;
    Ok(RunningChild {
        child,
        lease_id: owner_id,
        hostname: config.hostname.clone(),
        port,
        warning,
        tls: config.tls,
        bind_address: config.bind_address,
        readiness_warn_after: Duration::from_secs(config.readiness_warn_after_seconds),
        signals,
    })
}

fn pending_owner(kind: &PendingOperationKind) -> Option<Uuid> {
    match kind {
        PendingOperationKind::InstallRoute { owner_id, .. }
        | PendingOperationKind::RestoreRoute { owner_id, .. }
        | PendingOperationKind::RemoveRoute { owner_id, .. }
        | PendingOperationKind::StartProcess { owner_id, .. } => Some(*owner_id),
        PendingOperationKind::FinalizeLease { lease_id } => Some(*lease_id),
    }
}

impl RunningChild {
    pub(crate) fn wait_for_readiness(
        &mut self,
        store: &Store,
        mut warn: impl FnMut(&str),
    ) -> Result<bool, RunError> {
        let started = Instant::now();
        let mut warned = false;
        loop {
            for signal in self.signals.pending() {
                self.child.signal(signal)?;
            }
            if TcpStream::connect_timeout(
                &SocketAddr::new(readiness_probe_address(self.bind_address), self.port),
                Duration::from_millis(100),
            )
            .is_ok()
            {
                store.mutate(|registry| {
                    if let Some(lease) = registry.leases.get_mut(&self.lease_id) {
                        lease.state = LeaseState::Ready;
                    }
                    Ok(())
                })?;
                return Ok(true);
            }
            if self.child.has_exited()? {
                return Ok(false);
            }
            if !warned && started.elapsed() >= self.readiness_warn_after {
                warn(&format!(
                    "{} is still not accepting connections on port {}; its route and process remain active (check that it binds HOST and PORT)",
                    self.hostname, self.port
                ));
                warned = true;
            }
            thread::sleep(Duration::from_millis(50));
        }
    }

    pub(crate) fn finish(
        &mut self,
        store: &Store,
        routes: &mut impl RouteBackend,
    ) -> Result<RunOutcome, RunError> {
        let exit_code = self.child.wait_with_signals(&mut self.signals)?;
        let _operations = store.lock_operations()?;
        let cleanup = routes.remove_if_owned(&self.hostname, self.lease_id, self.tls);
        let mut warnings = Vec::new();
        store.mutate(|registry| {
            registry.leases.remove(&self.lease_id);
            if let Err(error) = &cleanup {
                registry.pending_operations.push(PendingOperation {
                    id: Uuid::new_v4(),
                    kind: PendingOperationKind::RemoveRoute {
                        hostname: self.hostname.clone(),
                        owner_id: self.lease_id,
                        tls: self.tls,
                    },
                });
                warnings.push(format!(
                    "cleanup of {} is pending: {error}; run `nook prune` to retry",
                    self.hostname
                ));
            }
            Ok(())
        })?;
        Ok(RunOutcome {
            exit_code,
            warnings,
        })
    }
}

fn readiness_probe_address(bind_address: IpAddr) -> IpAddr {
    match bind_address {
        IpAddr::V4(address) if address.is_unspecified() => IpAddr::V4(Ipv4Addr::LOCALHOST),
        IpAddr::V6(address) if address.is_unspecified() => IpAddr::V6(Ipv6Addr::LOCALHOST),
        address => address,
    }
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct RunOutcome {
    pub(crate) exit_code: i32,
    pub(crate) warnings: Vec<String>,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum StopError {
    State(String),
    NotManaged(String),
    Stale(String),
    Signal(String),
}

impl fmt::Display for StopError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::State(error) => write!(formatter, "cannot read managed runs: {error}"),
            Self::NotManaged(hostname) => {
                write!(formatter, "no managed run exists for `{hostname}`")
            }
            Self::Stale(hostname) => write!(
                formatter,
                "managed run `{hostname}` is no longer the same process"
            ),
            Self::Signal(error) => {
                write!(formatter, "cannot signal managed process group: {error}")
            }
        }
    }
}

impl std::error::Error for StopError {}

pub(crate) trait StopSystem {
    fn liveness(&mut self, lease: &Lease) -> Liveness;
    fn signal(&mut self, lease: &Lease, signal: ProcessSignal) -> io::Result<()>;
    fn sleep(&mut self, duration: Duration);
}

pub(crate) struct NativeStopSystem;

impl StopSystem for NativeStopSystem {
    fn liveness(&mut self, lease: &Lease) -> Liveness {
        lease_liveness(lease)
    }
    fn signal(&mut self, lease: &Lease, signal: ProcessSignal) -> io::Result<()> {
        signal_lease(lease, signal)
    }
    fn sleep(&mut self, duration: Duration) {
        thread::sleep(duration);
    }
}

pub(crate) fn stop_managed(
    store: &Store,
    hostname: &str,
    force: bool,
    system: &mut impl StopSystem,
) -> Result<ProcessSignal, StopError> {
    let _operations = store
        .lock_operations()
        .map_err(|error| StopError::State(error.to_string()))?;
    let lease = store
        .mutate(|registry| {
            Ok(registry
                .leases
                .values()
                .find(|lease| lease.hostname == hostname)
                .cloned())
        })
        .map_err(|error: crate::state::Error| StopError::State(error.to_string()))?;
    let lease = lease
        .as_ref()
        .ok_or_else(|| StopError::NotManaged(hostname.to_owned()))?;
    if system.liveness(lease) != Liveness::Alive {
        return Err(StopError::Stale(hostname.to_owned()));
    }
    if let Err(error) = system.signal(lease, ProcessSignal::Terminate) {
        if force {
            system
                .signal(lease, ProcessSignal::Kill)
                .map_err(|kill_error| StopError::Signal(kill_error.to_string()))?;
            return Ok(ProcessSignal::Kill);
        }
        return Err(StopError::Signal(error.to_string()));
    }
    if !force {
        return Ok(ProcessSignal::Terminate);
    }
    for _ in 0..40 {
        if system.liveness(lease) != Liveness::Alive {
            return Ok(ProcessSignal::Terminate);
        }
        system.sleep(Duration::from_millis(50));
    }
    if system.liveness(lease) == Liveness::Alive {
        system
            .signal(lease, ProcessSignal::Kill)
            .map_err(|error| StopError::Signal(error.to_string()))?;
        return Ok(ProcessSignal::Kill);
    }
    Ok(ProcessSignal::Terminate)
}

fn replace_operation(registry: &mut crate::state::Registry, id: Uuid, kind: PendingOperationKind) {
    if let Some(operation) = registry
        .pending_operations
        .iter_mut()
        .find(|operation| operation.id == id)
    {
        operation.kind = kind;
    }
}

pub(crate) fn spawn_child(
    argv: &[OsString],
    environment: &[(OsString, OsString)],
) -> Result<ManagedChild, Error> {
    spawn_managed_child(argv, environment, Uuid::new_v4())
}

fn spawn_managed_child(
    argv: &[OsString],
    environment: &[(OsString, OsString)],
    lease_id: Uuid,
) -> Result<ManagedChild, Error> {
    #[cfg(unix)]
    let _ = lease_id;
    let (program, arguments) = argv.split_first().ok_or(Error::EmptyCommand)?;
    let program = program.clone();
    let arguments = arguments.to_vec();
    #[cfg(windows)]
    let PreparedWindowsCommand {
        program,
        arguments,
        raw_argument,
        internal_environment,
    } = prepare_windows_command(program, arguments, environment, lease_id);
    let mut command = Command::new(program);
    command.args(arguments).envs(environment.iter().cloned());
    #[cfg(windows)]
    if let Some((key, value)) = internal_environment {
        command.env(key, value);
    }
    #[cfg(windows)]
    if let Some(raw_argument) = raw_argument {
        use std::os::windows::process::CommandExt;
        command.raw_arg(raw_argument);
    }
    command
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());
    configure_child(&mut command);
    let mut child = command.spawn().map_err(Error::Spawn)?;
    let pid = child.id();
    let Some(identity) = read_process_identity(pid) else {
        let _ = child.kill();
        let _ = child.wait();
        return Err(Error::ProcessIdentity(pid));
    };
    #[cfg(windows)]
    let job = match create_and_assign_job(&child, lease_id) {
        Ok(job) => job,
        Err(error) => {
            let _ = child.kill();
            let _ = child.wait();
            return Err(Error::Spawn(error));
        }
    };
    #[cfg(windows)]
    if let Err(error) = resume_process(&child) {
        drop_job_handle(job);
        let _ = child.wait();
        return Err(Error::Spawn(error));
    }
    Ok(ManagedChild {
        child,
        pid,
        pgid: identity.pgid,
        start_time_ticks: identity.start_time_ticks,
        #[cfg(windows)]
        job,
    })
}

#[cfg(unix)]
#[allow(unsafe_code)]
fn configure_child(command: &mut Command) {
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

#[cfg(windows)]
struct PreparedWindowsCommand {
    program: OsString,
    arguments: Vec<OsString>,
    raw_argument: Option<OsString>,
    internal_environment: Option<(OsString, OsString)>,
}

#[cfg(windows)]
fn prepare_windows_command(
    program: OsString,
    arguments: Vec<OsString>,
    environment: &[(OsString, OsString)],
    lease_id: Uuid,
) -> PreparedWindowsCommand {
    let Some(resolved) = resolve_windows_program(&program, environment) else {
        return PreparedWindowsCommand {
            program,
            arguments,
            raw_argument: None,
            internal_environment: None,
        };
    };
    let is_batch = resolved
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            extension.eq_ignore_ascii_case("cmd") || extension.eq_ignore_ascii_case("bat")
        });
    if !is_batch {
        return PreparedWindowsCommand {
            program: resolved.into_os_string(),
            arguments,
            raw_argument: None,
            internal_environment: None,
        };
    }

    let interpreter =
        effective_windows_env(environment, "COMSPEC").unwrap_or_else(|| OsString::from("cmd.exe"));
    let shell_arguments = vec![
        OsString::from("/D"),
        OsString::from("/V:OFF"),
        OsString::from("/S"),
        OsString::from("/C"),
    ];
    let percent_variable = format!("NOOK_INTERNAL_PERCENT_{}", lease_id.simple());
    let command_line =
        windows_batch_command_line(resolved.as_os_str(), &arguments, &percent_variable);
    PreparedWindowsCommand {
        program: interpreter,
        arguments: shell_arguments,
        raw_argument: Some(command_line),
        internal_environment: Some((OsString::from(percent_variable), OsString::from("%"))),
    }
}

#[cfg(windows)]
fn windows_batch_command_line(
    program: &std::ffi::OsStr,
    arguments: &[OsString],
    percent_variable: &str,
) -> OsString {
    use std::os::windows::ffi::{OsStrExt, OsStringExt};

    let mut command_line = vec![b'"' as u16];
    let percent_reference = format!("%{percent_variable}%")
        .encode_utf16()
        .collect::<Vec<_>>();
    for (index, argument) in std::iter::once(program)
        .chain(arguments.iter().map(OsString::as_os_str))
        .enumerate()
    {
        if index != 0 {
            command_line.push(b' ' as u16);
        }
        let quoted = index == 0
            || argument.encode_wide().next().is_none()
            || argument
                .encode_wide()
                .any(|unit| unit <= u16::from(u8::MAX) && b" \t\"&|<>^()%".contains(&(unit as u8)));
        if quoted {
            command_line.push(b'"' as u16);
        }
        for unit in argument.encode_wide() {
            if unit == b'%' as u16 {
                // cmd.exe expands variables exactly once. Expanding this
                // private variable inserts a percent sign too late for a
                // user argument such as %PATH% to be expanded recursively.
                command_line.extend_from_slice(&percent_reference);
            } else {
                command_line.push(unit);
            }
            if unit == b'"' as u16 {
                command_line.push(unit);
            }
        }
        if quoted {
            command_line.push(b'"' as u16);
        }
    }
    command_line.push(b'"' as u16);
    OsString::from_wide(&command_line)
}

#[cfg(windows)]
fn resolve_windows_program(
    program: &std::ffi::OsStr,
    environment: &[(OsString, OsString)],
) -> Option<std::path::PathBuf> {
    use std::path::{Path, PathBuf};

    let program = Path::new(program);
    let has_directory = program.is_absolute()
        || program
            .parent()
            .is_some_and(|parent| !parent.as_os_str().is_empty());
    let directories = if has_directory {
        vec![PathBuf::new()]
    } else {
        let mut directories = vec![env::current_dir().ok()?];
        if let Some(path) = effective_windows_env(environment, "PATH") {
            directories.extend(env::split_paths(&path));
        }
        directories
    };
    let extensions = if program.extension().is_some() {
        vec![OsString::new()]
    } else {
        effective_windows_env(environment, "PATHEXT")
            .unwrap_or_else(|| OsString::from(".COM;.EXE;.BAT;.CMD"))
            .to_string_lossy()
            .split(';')
            .filter(|extension| !extension.is_empty())
            .map(OsString::from)
            .collect()
    };

    for directory in directories {
        let base = directory.join(program);
        if base.is_file() {
            return Some(base);
        }
        for extension in &extensions {
            let mut candidate = base.as_os_str().to_os_string();
            candidate.push(extension);
            let candidate = PathBuf::from(candidate);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}

#[cfg(windows)]
fn effective_windows_env(environment: &[(OsString, OsString)], name: &str) -> Option<OsString> {
    environment
        .iter()
        .rev()
        .find(|(key, _)| key.to_string_lossy().eq_ignore_ascii_case(name))
        .map(|(_, value)| value.clone())
        .or_else(|| env::var_os(name))
        .filter(|value| !value.is_empty())
}

#[cfg(windows)]
fn configure_child(command: &mut Command) {
    use std::os::windows::process::CommandExt;
    use windows_sys::Win32::System::Threading::{CREATE_NEW_PROCESS_GROUP, CREATE_SUSPENDED};

    // Keep the primary thread suspended until the process belongs to its Job
    // Object. Otherwise a fast child could launch descendants before the job
    // assignment and those descendants would escape process-tree cleanup.
    command.creation_flags(CREATE_NEW_PROCESS_GROUP | CREATE_SUSPENDED);
}

#[cfg(unix)]
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

#[cfg(unix)]
fn signal_managed_child(child: &ManagedChild, signal: ProcessSignal) -> io::Result<()> {
    let signal = match signal {
        ProcessSignal::Interrupt => libc::SIGINT,
        ProcessSignal::Terminate => libc::SIGTERM,
        ProcessSignal::Kill => libc::SIGKILL,
    };
    send_group_signal(child.pgid, signal)
}

#[cfg(unix)]
fn signal_lease(lease: &Lease, signal: ProcessSignal) -> io::Result<()> {
    let signal = match signal {
        ProcessSignal::Interrupt => libc::SIGINT,
        ProcessSignal::Terminate => libc::SIGTERM,
        ProcessSignal::Kill => libc::SIGKILL,
    };
    send_group_signal(lease.pgid, signal)
}

#[cfg(unix)]
fn read_process_identity(pid: u32) -> Option<ProcIdentity> {
    fs::read_to_string(format!("/proc/{pid}/stat"))
        .ok()
        .and_then(|stat| parse_stat(&stat))
}

#[cfg(windows)]
fn job_name(lease_id: Uuid) -> Vec<u16> {
    format!("Local\\Nook-{lease_id}")
        .encode_utf16()
        .chain(Some(0))
        .collect()
}

#[cfg(windows)]
#[allow(unsafe_code)]
fn create_and_assign_job(
    child: &Child,
    lease_id: Uuid,
) -> io::Result<windows_sys::Win32::Foundation::HANDLE> {
    use std::mem::{size_of, zeroed};
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
        JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
        SetInformationJobObject,
    };

    let name = job_name(lease_id);
    let job = unsafe { CreateJobObjectW(std::ptr::null(), name.as_ptr()) };
    if job.is_null() {
        return Err(io::Error::last_os_error());
    }
    let mut information: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { zeroed() };
    information.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
    let configured = unsafe {
        SetInformationJobObject(
            job,
            JobObjectExtendedLimitInformation,
            (&raw const information).cast(),
            u32::try_from(size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>())
                .expect("job information size fits u32"),
        )
    };
    let assigned = configured != 0
        && unsafe { AssignProcessToJobObject(job, child.as_raw_handle() as _) } != 0;
    if !assigned {
        let error = io::Error::last_os_error();
        unsafe { CloseHandle(job) };
        return Err(error);
    }
    Ok(job)
}

#[cfg(windows)]
#[allow(unsafe_code)]
fn resume_process(child: &Child) -> io::Result<()> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Foundation::HANDLE;

    #[link(name = "ntdll")]
    unsafe extern "system" {
        fn NtResumeProcess(process: HANDLE) -> i32;
    }

    let status = unsafe { NtResumeProcess(child.as_raw_handle() as HANDLE) };
    if status < 0 {
        Err(io::Error::other(format!(
            "NtResumeProcess failed with NTSTATUS 0x{:08x}",
            status as u32
        )))
    } else {
        Ok(())
    }
}

#[cfg(windows)]
#[allow(unsafe_code)]
fn drop_job_handle(job: windows_sys::Win32::Foundation::HANDLE) {
    unsafe { windows_sys::Win32::Foundation::CloseHandle(job) };
}

#[cfg(windows)]
#[allow(unsafe_code)]
fn signal_managed_child(child: &ManagedChild, signal: ProcessSignal) -> io::Result<()> {
    use windows_sys::Win32::System::Console::{CTRL_BREAK_EVENT, GenerateConsoleCtrlEvent};
    use windows_sys::Win32::System::JobObjects::TerminateJobObject;

    let succeeded = match signal {
        ProcessSignal::Interrupt | ProcessSignal::Terminate => unsafe {
            GenerateConsoleCtrlEvent(CTRL_BREAK_EVENT, child.pid)
        },
        ProcessSignal::Kill => unsafe { TerminateJobObject(child.job, 1) },
    };
    if succeeded == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[cfg(windows)]
#[allow(unsafe_code)]
fn generate_console_break(pid: u32) -> io::Result<()> {
    use windows_sys::Win32::System::Console::{
        ATTACH_PARENT_PROCESS, AttachConsole, CTRL_BREAK_EVENT, FreeConsole,
        GenerateConsoleCtrlEvent,
    };

    // A separate `nook stop` process normally belongs to the caller's console,
    // while the managed application may have been started in another one.
    // Temporarily join the application's console so CTRL_BREAK can reach the
    // process group created for it, then restore the caller's parent console.
    let had_console = unsafe { FreeConsole() } != 0;
    if unsafe { AttachConsole(pid) } == 0 {
        let error = io::Error::last_os_error();
        if had_console {
            unsafe { AttachConsole(ATTACH_PARENT_PROCESS) };
        }
        return Err(error);
    }

    let generated = unsafe { GenerateConsoleCtrlEvent(CTRL_BREAK_EVENT, pid) };
    let error = (generated == 0).then(io::Error::last_os_error);
    unsafe { FreeConsole() };
    if had_console {
        unsafe { AttachConsole(ATTACH_PARENT_PROCESS) };
    }
    error.map_or(Ok(()), Err)
}

#[cfg(windows)]
#[allow(unsafe_code)]
fn signal_lease(lease: &Lease, signal: ProcessSignal) -> io::Result<()> {
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::JobObjects::{OpenJobObjectW, TerminateJobObject};
    use windows_sys::Win32::System::SystemServices::JOB_OBJECT_TERMINATE;

    match signal {
        ProcessSignal::Interrupt | ProcessSignal::Terminate => generate_console_break(lease.pid),
        ProcessSignal::Kill => {
            let name = job_name(lease.id);
            let job = unsafe { OpenJobObjectW(JOB_OBJECT_TERMINATE, 0, name.as_ptr()) };
            if job.is_null() {
                return Err(io::Error::last_os_error());
            }
            let result = unsafe { TerminateJobObject(job, 1) };
            let error = (result == 0).then(io::Error::last_os_error);
            unsafe { CloseHandle(job) };
            error.map_or(Ok(()), Err)
        }
    }
}

#[cfg(windows)]
#[allow(unsafe_code)]
fn read_process_identity(pid: u32) -> Option<ProcIdentity> {
    use std::mem::zeroed;
    use windows_sys::Win32::Foundation::{CloseHandle, FILETIME};
    use windows_sys::Win32::System::Threading::{
        GetProcessTimes, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
    };

    let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if process.is_null() {
        return None;
    }
    let mut creation: FILETIME = unsafe { zeroed() };
    let mut exit: FILETIME = unsafe { zeroed() };
    let mut kernel: FILETIME = unsafe { zeroed() };
    let mut user: FILETIME = unsafe { zeroed() };
    let result =
        unsafe { GetProcessTimes(process, &mut creation, &mut exit, &mut kernel, &mut user) };
    unsafe { CloseHandle(process) };
    let pgid = i32::try_from(pid).ok()?;
    (result != 0).then(|| ProcIdentity {
        state: b'R',
        pgid,
        start_time_ticks: u64::from(creation.dwLowDateTime)
            | (u64::from(creation.dwHighDateTime) << 32),
    })
}

#[cfg(windows)]
impl Drop for ManagedChild {
    #[allow(unsafe_code)]
    fn drop(&mut self) {
        unsafe { windows_sys::Win32::Foundation::CloseHandle(self.job) };
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
    reserve_port_on(IpAddr::V4(Ipv4Addr::LOCALHOST), preferred, strict)
}

pub(crate) fn reserve_port_on(
    address: IpAddr,
    preferred: Option<u16>,
    strict: bool,
) -> Result<PortReservation, Error> {
    if preferred == Some(0) {
        return Err(Error::InvalidPort);
    }
    if let Some(port) = preferred {
        match bind(address, port) {
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
                let listener = bind(address, 0).map_err(Error::Bind)?;
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
    let listener = bind(address, 0).map_err(Error::Bind)?;
    let port = listener.local_addr().map_err(Error::Bind)?.port();
    Ok(PortReservation {
        listener,
        port,
        warning: None,
    })
}

fn bind(address: IpAddr, port: u16) -> io::Result<TcpListener> {
    TcpListener::bind(SocketAddr::new(address, port))
}

pub(crate) fn substitute_port(arguments: &[OsString], port: u16) -> Vec<OsString> {
    let replacement = port.to_string();
    substitute_port_platform(arguments, &replacement)
}

#[cfg(unix)]
fn substitute_port_platform(arguments: &[OsString], replacement: &str) -> Vec<OsString> {
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

#[cfg(windows)]
fn substitute_port_platform(arguments: &[OsString], replacement: &str) -> Vec<OsString> {
    use std::os::windows::ffi::{OsStrExt, OsStringExt};

    let needle: Vec<u16> = "{port}".encode_utf16().collect();
    let replacement: Vec<u16> = replacement.encode_utf16().collect();
    arguments
        .iter()
        .map(|argument| {
            let input: Vec<u16> = argument.encode_wide().collect();
            OsString::from_wide(&replace_wide(&input, &needle, &replacement))
        })
        .collect()
}

#[cfg(windows)]
fn replace_wide(input: &[u16], needle: &[u16], replacement: &[u16]) -> Vec<u16> {
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

#[cfg(unix)]
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

pub(crate) fn child_environment(
    port: u16,
    bind_address: IpAddr,
    hostname: &str,
    tls: bool,
) -> [(OsString, OsString); 3] {
    [
        (OsString::from("PORT"), OsString::from(port.to_string())),
        (
            OsString::from("HOST"),
            OsString::from(bind_address.to_string()),
        ),
        (
            OsString::from("NOOK_URL"),
            OsString::from(format!(
                "{}://{hostname}",
                if tls { "https" } else { "http" }
            )),
        ),
    ]
}

#[cfg(unix)]
pub(crate) fn lease_liveness(lease: &Lease) -> Liveness {
    match fs::read_to_string(format!("/proc/{}/stat", lease.pid)) {
        Ok(stat) => match parse_stat(&stat) {
            Some(identity) => {
                identity_liveness(identity, lease.pgid, lease.process_start_time_ticks)
            }
            None => Liveness::Indeterminate,
        },
        Err(error) if error.kind() == io::ErrorKind::NotFound => Liveness::Dead,
        Err(_) => Liveness::Indeterminate,
    }
}

#[cfg(windows)]
#[allow(unsafe_code)]
pub(crate) fn lease_liveness(lease: &Lease) -> Liveness {
    use windows_sys::Win32::Foundation::{CloseHandle, WAIT_TIMEOUT};
    use windows_sys::Win32::Storage::FileSystem::SYNCHRONIZE;
    use windows_sys::Win32::System::Threading::{
        OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, WaitForSingleObject,
    };

    let process = unsafe {
        OpenProcess(
            PROCESS_QUERY_LIMITED_INFORMATION | SYNCHRONIZE,
            0,
            lease.pid,
        )
    };
    if process.is_null() {
        return match io::Error::last_os_error().raw_os_error() {
            Some(87) | Some(1168) => Liveness::Dead,
            _ => Liveness::Indeterminate,
        };
    }
    let wait = unsafe { WaitForSingleObject(process, 0) };
    unsafe { CloseHandle(process) };
    if wait != WAIT_TIMEOUT {
        return Liveness::Dead;
    }
    match read_process_identity(lease.pid) {
        Some(identity) if identity.start_time_ticks == lease.process_start_time_ticks => {
            Liveness::Alive
        }
        Some(_) => Liveness::Dead,
        None => Liveness::Indeterminate,
    }
}

#[cfg(unix)]
fn identity_liveness(identity: ProcIdentity, pgid: i32, start_time_ticks: u64) -> Liveness {
    if matches!(identity.state, b'Z' | b'X')
        || identity.pgid != pgid
        || identity.start_time_ticks != start_time_ticks
    {
        Liveness::Dead
    } else {
        Liveness::Alive
    }
}

#[cfg(unix)]
fn parse_stat(stat: &str) -> Option<ProcIdentity> {
    let command_end = stat.rfind(')')?;
    let fields: Vec<_> = stat.get(command_end + 1..)?.split_whitespace().collect();
    Some(ProcIdentity {
        state: *fields.first()?.as_bytes().first()?,
        pgid: fields.get(2)?.parse().ok()?,
        start_time_ticks: fields.get(19)?.parse().ok()?,
    })
}

#[cfg(all(test, unix))]
mod tests {
    use std::collections::BTreeMap;
    use std::ffi::OsString;
    use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddrV4, TcpListener};
    use std::os::unix::ffi::{OsStrExt, OsStringExt};

    use super::{
        Error, Liveness, ProcIdentity, ProcessSignal, StopError, StopSystem, child_environment,
        identity_liveness, parse_stat, readiness_probe_address, reserve_port, spawn_child,
        start_run, start_run_with_hook, stop_managed, substitute_port,
    };
    use crate::config::ResolvedRunConfig;
    use crate::reconcile::{RouteBackend, RouteError, RouteSpec};
    use crate::state::{LeaseState, Store, decode};
    use uuid::Uuid;

    #[derive(Default)]
    struct Routes {
        owners: BTreeMap<String, Uuid>,
        unavailable_on_remove: bool,
        foreign: bool,
    }

    impl RouteBackend for Routes {
        fn ensure(&mut self, route: &RouteSpec) -> Result<(), RouteError> {
            if self.foreign {
                return Err(RouteError("foreign route".into()));
            }
            self.owners.insert(route.hostname.clone(), route.owner_id);
            Ok(())
        }

        fn remove_if_owned(
            &mut self,
            hostname: &str,
            owner_id: Uuid,
            _tls: bool,
        ) -> Result<(), RouteError> {
            if self.unavailable_on_remove {
                return Err(RouteError("unavailable".into()));
            }
            if self.owners.get(hostname) == Some(&owner_id) {
                self.owners.remove(hostname);
            }
            Ok(())
        }
    }

    #[test]
    fn readiness_uses_loopback_for_wildcard_bind_addresses() {
        assert_eq!(
            readiness_probe_address(IpAddr::V4(Ipv4Addr::UNSPECIFIED)),
            IpAddr::V4(Ipv4Addr::LOCALHOST)
        );
        assert_eq!(
            readiness_probe_address(IpAddr::V6(Ipv6Addr::UNSPECIFIED)),
            IpAddr::V6(Ipv6Addr::LOCALHOST)
        );
        let explicit = IpAddr::V4(Ipv4Addr::new(192, 0, 2, 10));
        assert_eq!(readiness_probe_address(explicit), explicit);
    }

    #[test]
    fn parses_pgid_and_start_time_even_when_comm_contains_spaces_and_parentheses() {
        let stat =
            "42 (worker (test) name) S 1 77 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 98765 21";
        assert_eq!(
            parse_stat(stat),
            Some(ProcIdentity {
                state: b'S',
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
    fn zombie_and_reused_process_identities_are_dead() {
        let identity = ProcIdentity {
            state: b'Z',
            pgid: 77,
            start_time_ticks: 98765,
        };
        assert_eq!(identity_liveness(identity, 77, 98765), Liveness::Dead);
        assert_eq!(
            identity_liveness(
                ProcIdentity {
                    state: b'S',
                    ..identity
                },
                77,
                98765,
            ),
            Liveness::Alive
        );
        assert_eq!(
            identity_liveness(
                ProcIdentity {
                    state: b'S',
                    ..identity
                },
                77,
                1,
            ),
            Liveness::Dead
        );
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
        let environment =
            child_environment(3000, IpAddr::V4(Ipv4Addr::LOCALHOST), "api.localhost", true);
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
        child.signal(ProcessSignal::Terminate).unwrap();
        assert_eq!(child.wait().unwrap(), 128 + libc::SIGTERM);
    }

    #[test]
    fn route_precedes_spawn_and_lease_is_starting_before_readiness() {
        let (store, path) = temporary_store();
        let config = run_config(vec!["/bin/sleep", "10"], 30);
        let mut routes = Routes::default();
        let mut running = start_run(&config, &store, &mut routes).unwrap();
        let registry = decode(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(
            registry.leases[&running.lease_id].state,
            LeaseState::Starting
        );
        assert_eq!(routes.owners.get("api.localhost"), Some(&running.lease_id));
        assert!(registry.pending_operations.is_empty());
        running.child.signal(ProcessSignal::Terminate).unwrap();
        assert_eq!(running.finish(&store, &mut routes).unwrap().exit_code, 143);
        assert!(
            decode(&std::fs::read(&path).unwrap())
                .unwrap()
                .leases
                .is_empty()
        );
        assert!(routes.owners.is_empty());
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn readiness_marks_lease_ready_after_real_tcp_acceptor_starts() {
        let (store, path) = temporary_store();
        let code = "import os,socket,time;time.sleep(.1);s=socket.socket();s.setsockopt(socket.SOL_SOCKET,socket.SO_REUSEADDR,1);s.bind(('127.0.0.1',int(os.environ['PORT'])));s.listen();time.sleep(10)";
        let config = run_config(vec!["/usr/bin/python3", "-c", code], 2);
        let mut routes = Routes::default();
        let mut running = start_run(&config, &store, &mut routes).unwrap();
        assert!(
            running
                .wait_for_readiness(&store, |_| panic!("unexpected warning"))
                .unwrap()
        );
        let registry = decode(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(registry.leases[&running.lease_id].state, LeaseState::Ready);
        running.child.signal(ProcessSignal::Terminate).unwrap();
        running.child.wait().unwrap();
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn failed_spawn_removes_route_and_provisional_operation() {
        let (store, path) = temporary_store();
        let config = run_config(vec!["/definitely/missing/nook-command"], 30);
        let mut routes = Routes::default();
        assert!(start_run(&config, &store, &mut routes).is_err());
        let registry = decode(&std::fs::read(&path).unwrap()).unwrap();
        assert!(registry.leases.is_empty());
        assert!(registry.pending_operations.is_empty());
        assert!(routes.owners.is_empty());
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn existing_managed_hostname_requires_force() {
        let (store, path) = temporary_store();
        let mut routes = Routes::default();
        let config = run_config(vec!["/bin/sleep", "10"], 30);
        let mut first = start_run(&config, &store, &mut routes).unwrap();
        assert!(matches!(
            start_run(&config, &store, &mut routes),
            Err(super::RunError::Conflict(hostname)) if hostname == "api.localhost"
        ));
        first.child.signal(ProcessSignal::Terminate).unwrap();
        first.finish(&store, &mut routes).unwrap();
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn force_transfers_ownership_without_stopping_previous_run() {
        let (store, path) = temporary_store();
        let mut routes = Routes::default();
        let mut first = start_run(
            &run_config(vec!["/bin/sleep", "10"], 30),
            &store,
            &mut routes,
        )
        .unwrap();
        let mut replacement_config = run_config(vec!["/bin/sleep", "10"], 30);
        replacement_config.force = true;
        let mut replacement = start_run(&replacement_config, &store, &mut routes).unwrap();
        assert!(
            replacement
                .warning
                .as_deref()
                .unwrap()
                .contains("previous owner")
        );
        assert_eq!(routes.owners["api.localhost"], replacement.lease_id);
        let registry = decode(&std::fs::read(&path).unwrap()).unwrap();
        assert!(!registry.leases.contains_key(&first.lease_id));
        assert!(registry.leases.contains_key(&replacement.lease_id));

        first.child.signal(ProcessSignal::Terminate).unwrap();
        first.finish(&store, &mut routes).unwrap();
        assert_eq!(routes.owners["api.localhost"], replacement.lease_id);
        assert!(
            decode(&std::fs::read(&path).unwrap())
                .unwrap()
                .leases
                .contains_key(&replacement.lease_id)
        );
        replacement.child.signal(ProcessSignal::Terminate).unwrap();
        replacement.finish(&store, &mut routes).unwrap();
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn force_never_bypasses_a_foreign_route_rejection() {
        let (store, path) = temporary_store();
        let mut routes = Routes {
            foreign: true,
            ..Routes::default()
        };
        let mut config = run_config(vec!["/bin/sleep", "10"], 30);
        config.force = true;
        assert!(matches!(
            start_run(&config, &store, &mut routes),
            Err(super::RunError::Route(_))
        ));
        assert!(routes.owners.is_empty());
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn readiness_warns_once_but_waits_until_child_exits() {
        let (store, path) = temporary_store();
        let config = run_config(vec!["/bin/sleep", "0.2"], 0);
        let mut routes = Routes::default();
        let mut running = start_run(&config, &store, &mut routes).unwrap();
        let mut warnings = 0;
        assert!(
            !running
                .wait_for_readiness(&store, |_| warnings += 1)
                .unwrap()
        );
        assert_eq!(warnings, 1);
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn caddy_cleanup_failure_preserves_exit_code_and_persists_retry() {
        let (store, path) = temporary_store();
        let config = run_config(vec!["/bin/sh", "-c", "exit 7"], 30);
        let mut routes = Routes::default();
        let mut running = start_run(&config, &store, &mut routes).unwrap();
        routes.unavailable_on_remove = true;
        let outcome = running.finish(&store, &mut routes).unwrap();
        assert_eq!(outcome.exit_code, 7);
        assert_eq!(outcome.warnings.len(), 1);
        let registry = decode(&std::fs::read(&path).unwrap()).unwrap();
        assert!(registry.leases.is_empty());
        assert_eq!(registry.pending_operations.len(), 1);
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn successful_cleanup_is_idempotent() {
        let (store, path) = temporary_store();
        let config = run_config(vec!["/bin/sh", "-c", "exit 0"], 30);
        let mut routes = Routes::default();
        let mut running = start_run(&config, &store, &mut routes).unwrap();
        assert_eq!(running.finish(&store, &mut routes).unwrap().exit_code, 0);
        assert_eq!(running.finish(&store, &mut routes).unwrap().exit_code, 0);
        let registry = decode(&std::fs::read(&path).unwrap()).unwrap();
        assert!(registry.leases.is_empty());
        assert!(registry.pending_operations.is_empty());
        assert!(routes.owners.is_empty());
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn lost_port_race_does_not_restart_or_leave_route_or_lease() {
        use std::cell::RefCell;
        let (store, path) = temporary_store();
        let code =
            "import os,socket;s=socket.socket();s.bind(('127.0.0.1',int(os.environ['PORT'])))";
        let config = run_config(vec!["/usr/bin/python3", "-c", code], 30);
        let stolen = RefCell::new(None);
        let mut routes = Routes::default();
        let mut running = start_run_with_hook(&config, &store, &mut routes, |port| {
            *stolen.borrow_mut() = Some(TcpListener::bind((Ipv4Addr::LOCALHOST, port)).unwrap());
        })
        .unwrap();
        let outcome = running.finish(&store, &mut routes).unwrap();
        assert_ne!(outcome.exit_code, 0);
        assert!(
            decode(&std::fs::read(&path).unwrap())
                .unwrap()
                .leases
                .is_empty()
        );
        assert!(routes.owners.is_empty());
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[derive(Default)]
    struct ControlledStop {
        alive_checks: usize,
        signals: Vec<ProcessSignal>,
        sleeps: usize,
        stale: bool,
        terminate_fails: bool,
    }

    impl StopSystem for ControlledStop {
        fn liveness(&mut self, _lease: &crate::state::Lease) -> Liveness {
            self.alive_checks += 1;
            if self.stale {
                Liveness::Dead
            } else {
                Liveness::Alive
            }
        }
        fn signal(
            &mut self,
            _lease: &crate::state::Lease,
            signal: ProcessSignal,
        ) -> std::io::Result<()> {
            self.signals.push(signal);
            if signal == ProcessSignal::Terminate && self.terminate_fails {
                return Err(std::io::Error::other("graceful signal unavailable"));
            }
            Ok(())
        }
        fn sleep(&mut self, _duration: std::time::Duration) {
            self.sleeps += 1;
        }
    }

    #[test]
    fn force_stop_waits_two_controlled_seconds_then_kills_group() {
        let (store, path) = temporary_store();
        let config = run_config(vec!["/bin/sleep", "10"], 30);
        let mut routes = Routes::default();
        let mut running = start_run(&config, &store, &mut routes).unwrap();
        let mut system = ControlledStop::default();
        let signal = stop_managed(&store, "api.localhost", true, &mut system).unwrap();
        assert_eq!(signal, ProcessSignal::Kill);
        assert_eq!(
            system.signals,
            [ProcessSignal::Terminate, ProcessSignal::Kill]
        );
        assert_eq!(system.sleeps, 40);
        running.child.signal(ProcessSignal::Terminate).unwrap();
        running.child.wait().unwrap();
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn force_stop_kills_immediately_when_graceful_signal_is_unavailable() {
        let (store, path) = temporary_store();
        let config = run_config(vec!["/bin/sleep", "10"], 30);
        let mut routes = Routes::default();
        let mut running = start_run(&config, &store, &mut routes).unwrap();
        let mut system = ControlledStop {
            terminate_fails: true,
            ..ControlledStop::default()
        };
        let signal = stop_managed(&store, "api.localhost", true, &mut system).unwrap();
        assert_eq!(signal, ProcessSignal::Kill);
        assert_eq!(
            system.signals,
            [ProcessSignal::Terminate, ProcessSignal::Kill]
        );
        assert_eq!(system.sleeps, 0);
        running.child.signal(ProcessSignal::Terminate).unwrap();
        running.child.wait().unwrap();
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn stale_managed_identity_is_never_signalled() {
        let (store, path) = temporary_store();
        let config = run_config(vec!["/bin/sleep", "10"], 30);
        let mut routes = Routes::default();
        let mut running = start_run(&config, &store, &mut routes).unwrap();
        let mut system = ControlledStop {
            stale: true,
            ..ControlledStop::default()
        };
        assert!(stop_managed(&store, "api.localhost", true, &mut system).is_err());
        assert!(system.signals.is_empty());
        running.child.signal(ProcessSignal::Terminate).unwrap();
        running.child.wait().unwrap();
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn unmanaged_hostname_is_never_signalled() {
        let (store, path) = temporary_store();
        let mut system = ControlledStop::default();
        assert_eq!(
            stop_managed(&store, "missing.localhost", true, &mut system),
            Err(StopError::NotManaged("missing.localhost".into()))
        );
        assert!(system.signals.is_empty());
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    fn run_config(command: Vec<&str>, readiness: u64) -> ResolvedRunConfig {
        ResolvedRunConfig {
            hostname: "api.localhost".into(),
            command: command.into_iter().map(OsString::from).collect(),
            tls: true,
            app_port: None,
            strict_port: false,
            force: false,
            readiness_warn_after_seconds: readiness,
            bind_address: IpAddr::V4(Ipv4Addr::LOCALHOST),
            ignored_local_config: None,
        }
    }

    fn temporary_store() -> (Store, std::path::PathBuf) {
        let directory = std::env::temp_dir().join(format!("nook-run-{}", Uuid::new_v4()));
        let path = directory.join("state.json");
        (Store::new(path.clone()), path)
    }
}

#[cfg(all(test, windows))]
mod windows_tests {
    use std::ffi::OsString;
    use std::os::windows::ffi::{OsStrExt, OsStringExt};

    use super::{
        Liveness, ProcessSignal, lease_liveness, read_process_identity, spawn_child,
        substitute_port,
    };
    use crate::state::{Lease, LeaseState, Scheme};
    use uuid::Uuid;

    #[test]
    fn substitution_preserves_unpaired_utf16() {
        let argument = OsString::from_wide(&[
            0xd800,
            b'-' as u16,
            b'{' as u16,
            b'p' as u16,
            b'o' as u16,
            b'r' as u16,
            b't' as u16,
            b'}' as u16,
        ]);
        let replaced = substitute_port(&[argument], 4321);
        assert_eq!(
            replaced[0].encode_wide().collect::<Vec<_>>(),
            [
                0xd800,
                b'-' as u16,
                b'4' as u16,
                b'3' as u16,
                b'2' as u16,
                b'1' as u16
            ]
        );
    }

    #[test]
    fn child_runs_in_a_named_job_and_preserves_exit_code() {
        let mut child = spawn_child(
            &[
                OsString::from("cmd.exe"),
                OsString::from("/D"),
                OsString::from("/C"),
                OsString::from("exit 7"),
            ],
            &[],
        )
        .unwrap();
        assert_eq!(child.pgid, child.pid as i32);
        assert!(child.start_time_ticks > 0);
        assert_eq!(child.wait().unwrap(), 7);
    }

    #[test]
    fn path_resolves_cmd_package_shims() {
        let directory = std::env::temp_dir().join(format!("nook command shim {}", Uuid::new_v4()));
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::write(
            directory.join("nook-test-shim.cmd"),
            concat!(
                "@if \"%~1\"==\"7\" if \"%~2\"==\"%%NOOK_TEST_SHIM_VALUE%%\" ",
                "(exit /b 7) else (exit /b 9)\r\n",
            ),
        )
        .unwrap();
        let mut child = spawn_child(
            &[
                OsString::from("nook-test-shim"),
                OsString::from("7"),
                OsString::from("%NOOK_TEST_SHIM_VALUE%"),
            ],
            &[
                (OsString::from("PATH"), directory.as_os_str().to_owned()),
                (OsString::from("PATHEXT"), OsString::from(".CMD")),
                (
                    OsString::from("NOOK_TEST_SHIM_VALUE"),
                    OsString::from("expanded"),
                ),
            ],
        )
        .unwrap();
        assert_eq!(child.wait().unwrap(), 7);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn force_terminates_the_managed_job() {
        let mut child = spawn_child(
            &[
                OsString::from("cmd.exe"),
                OsString::from("/D"),
                OsString::from("/C"),
                OsString::from("ping -n 30 127.0.0.1 >NUL"),
            ],
            &[],
        )
        .unwrap();
        child.signal(ProcessSignal::Kill).unwrap();
        assert_ne!(child.wait().unwrap(), 0);
    }

    #[test]
    fn process_creation_time_prevents_pid_reuse() {
        let pid = std::process::id();
        let identity = read_process_identity(pid).unwrap();
        let mut lease = Lease {
            id: Uuid::new_v4(),
            hostname: "test.localhost".into(),
            target: "http://127.0.0.1:3000".into(),
            scheme: Scheme::Http,
            tls: false,
            pid,
            pgid: identity.pgid,
            process_start_time_ticks: identity.start_time_ticks,
            state: LeaseState::Ready,
        };
        assert_eq!(lease_liveness(&lease), Liveness::Alive);
        lease.process_start_time_ticks = lease.process_start_time_ticks.saturating_add(1);
        assert_eq!(lease_liveness(&lease), Liveness::Dead);
    }
}
