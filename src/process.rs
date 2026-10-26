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
use crate::reconcile::{RouteBackend, RouteError, RouteSpec, route_for_alias, route_for_lease};
use crate::state::{Lease, WorkspaceSupervisor};
use crate::state::{
    LeaseState, OperationGuard, PendingOperation, PendingOperationKind, Scheme, Store,
};
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Liveness {
    Alive,
    Dead,
    Indeterminate,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ProcIdentity {
    pub(crate) state: u8,
    pub(crate) pgid: i32,
    pub(crate) start_time_ticks: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ProcessSignal {
    Interrupt,
    Terminate,
    Kill,
}

pub(crate) struct ForwardedSignals {
    #[cfg(unix)]
    inner: Signals,
}

#[cfg(windows)]
static WINDOWS_INTERRUPTED: AtomicBool = AtomicBool::new(false);
#[cfg(windows)]
static WINDOWS_SIGNAL_HANDLER: OnceLock<Result<(), String>> = OnceLock::new();

impl ForwardedSignals {
    pub(crate) fn new() -> io::Result<Self> {
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

    pub(crate) fn pending(&mut self) -> Vec<ProcessSignal> {
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
            Self::Bind(error) => write!(formatter, "cannot reserve an application port: {error}"),
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
    output_threads: Vec<thread::JoinHandle<()>>,
    pub(crate) pid: u32,
    pub(crate) pgid: i32,
    pub(crate) start_time_ticks: u64,
    #[cfg(unix)]
    workspace_owned: bool,
    #[cfg(unix)]
    terminal: Option<TerminalForeground>,
    #[cfg(unix)]
    terminal_job_control: bool,
    #[cfg(windows)]
    job: windows_sys::Win32::Foundation::HANDLE,
}

impl ManagedChild {
    pub(crate) fn signal(&self, signal: ProcessSignal) -> Result<(), Error> {
        signal_managed_child(self, signal)
            .map(|_| ())
            .map_err(Error::Spawn)
    }

    /// Only actual group delivery may suppress a workspace's marker scan.
    #[cfg(target_os = "linux")]
    pub(crate) fn signal_workspace_group(&self, signal: ProcessSignal) -> Result<bool, Error> {
        match signal_managed_child(self, signal) {
            Err(error) if error.raw_os_error() == Some(libc::ESRCH) => Ok(false),
            result => result.map_err(Error::Spawn),
        }
    }

    pub(crate) fn wait(&mut self) -> Result<i32, Error> {
        let mut signals = ForwardedSignals::new().map_err(Error::Spawn)?;
        self.wait_with_signals(&mut signals, || {})
    }

    pub(crate) fn signal_foreground(&self, signal: ProcessSignal) -> Result<(), Error> {
        #[cfg(unix)]
        {
            self.signal(signal)
        }
        #[cfg(windows)]
        {
            signal_with_forced_fallback(signal, |signal| signal_managed_child(self, signal))
                .map_err(Error::Spawn)
        }
    }

    fn wait_with_signals(
        &mut self,
        signals: &mut ForwardedSignals,
        mut on_tick: impl FnMut(),
    ) -> Result<i32, Error> {
        loop {
            on_tick();
            for signal in signals.pending() {
                #[cfg(unix)]
                let _ = self.signal(signal);
                #[cfg(windows)]
                self.signal_foreground(signal)?;
            }
            if let Some(status) = self.child.try_wait().map_err(Error::Spawn)? {
                #[cfg(unix)]
                drop(self.terminal.take());
                #[cfg(unix)]
                return Ok(status
                    .code()
                    .unwrap_or_else(|| 128 + status.signal().unwrap_or(1)));
                #[cfg(windows)]
                return Ok(status.code().unwrap_or(1));
            }
            #[cfg(unix)]
            self.sync_terminal();
            thread::sleep(Duration::from_millis(20));
        }
    }

    pub(crate) fn join_output(&mut self) {
        for output in self.output_threads.drain(..) {
            let _ = output.join();
        }
    }

    pub(crate) fn try_wait(&mut self) -> Result<Option<i32>, Error> {
        let status = self.child.try_wait().map_err(Error::Spawn)?;
        #[cfg(unix)]
        if status.is_some() {
            drop(self.terminal.take());
        } else {
            self.sync_terminal();
        }
        Ok(status.map(|status| {
            #[cfg(unix)]
            {
                status
                    .code()
                    .unwrap_or_else(|| 128 + status.signal().unwrap_or(1))
            }
            #[cfg(windows)]
            {
                status.code().unwrap_or(1)
            }
        }))
    }

    fn has_exited(&mut self) -> Result<bool, Error> {
        self.child
            .try_wait()
            .map(|status| status.is_some())
            .map_err(Error::Spawn)
    }

    #[cfg(unix)]
    fn sync_terminal(&mut self) {
        if !self.terminal_job_control {
            return;
        }
        if self.terminal.is_none() && !process_is_stopped(self.pid) {
            self.terminal = TerminalForeground::claim(self.pgid);
        }
        self.follow_stopped_child();
    }

    #[cfg(unix)]
    #[allow(unsafe_code)]
    fn follow_stopped_child(&mut self) {
        if !process_is_stopped(self.pid) {
            return;
        }
        drop(self.terminal.take());
        // SAFETY: unblock SIGTSTP, restore the default stop action, and
        // stop this process group so a pipeline job (`nook | cat`) suspends
        // together. After SIGCONT we re-claim the TTY and continue the child.
        unsafe {
            let mut set = std::mem::zeroed::<libc::sigset_t>();
            libc::sigemptyset(&mut set);
            libc::sigaddset(&mut set, libc::SIGTSTP);
            libc::sigprocmask(libc::SIG_UNBLOCK, &set, std::ptr::null_mut());
            libc::signal(libc::SIGTSTP, libc::SIG_DFL);
            libc::kill(0, libc::SIGTSTP);
        }
        self.terminal = TerminalForeground::claim(self.pgid);
        let _ = send_group_signal(self.pgid, libc::SIGCONT);
    }
}

#[derive(Debug)]
pub(crate) enum RunError {
    State(crate::state::Error),
    Route(RouteError),
    Process(Error),
    Exposure(Box<dyn std::error::Error + Send + Sync>),
    Conflict(String),
    LanHostnameConflict { hostname: String, project: String },
}

impl fmt::Display for RunError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::State(error) => error.fmt(formatter),
            Self::Route(error) => error.fmt(formatter),
            Self::Process(error) => error.fmt(formatter),
            Self::Exposure(error) => error.fmt(formatter),
            Self::Conflict(hostname) => write!(
                formatter,
                "hostname `{hostname}` is already managed by Nook; use --force to replace it"
            ),
            Self::LanHostnameConflict { hostname, project } => write!(
                formatter,
                "LAN hostname `{hostname}` is already used by `{project}`; choose another --name (even with --force)"
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
            Self::Exposure(error) => Some(error.as_ref()),
            Self::Conflict(_) | Self::LanHostnameConflict { .. } => None,
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
    lan: Option<crate::lan::Advertisement>,
}

/// Additional exposure of a run beyond its local Caddy route.
///
/// `expose` runs with the operations lock held, after the route exists and
/// before the child is spawned; the returned variables are added to the child
/// environment. `withdraw` undoes it when the child cannot be spawned.
pub(crate) trait RunExposure {
    fn expose(
        &mut self,
        operations: &OperationGuard,
        store: &Store,
        owner_id: Uuid,
        hostname: &str,
        target: &str,
    ) -> Result<Vec<(OsString, OsString)>, RunError>;
    fn withdraw(&mut self, operations: &OperationGuard, store: &Store, owner_id: Uuid);
}

pub(crate) struct LocalOnly;

impl RunExposure for LocalOnly {
    fn expose(
        &mut self,
        _operations: &OperationGuard,
        _store: &Store,
        _owner_id: Uuid,
        _hostname: &str,
        _target: &str,
    ) -> Result<Vec<(OsString, OsString)>, RunError> {
        Ok(Vec::new())
    }

    fn withdraw(&mut self, _operations: &OperationGuard, _store: &Store, _owner_id: Uuid) {}
}

pub(crate) fn start_run(
    config: &ResolvedRunConfig,
    store: &Store,
    routes: &mut impl RouteBackend,
) -> Result<RunningChild, RunError> {
    start_run_with_hook(config, store, routes, &mut LocalOnly, |_| {})
}

pub(crate) fn start_exposed_run(
    config: &ResolvedRunConfig,
    store: &Store,
    routes: &mut impl RouteBackend,
    exposure: &mut impl RunExposure,
) -> Result<RunningChild, RunError> {
    start_run_with_hook(config, store, routes, exposure, |_| {})
}

fn start_run_with_hook(
    config: &ResolvedRunConfig,
    store: &Store,
    routes: &mut impl RouteBackend,
    exposure: &mut impl RunExposure,
    after_release: impl FnOnce(u16),
) -> Result<RunningChild, RunError> {
    let signals = ForwardedSignals::new().map_err(Error::Spawn)?;
    let operations = store.lock_operations()?;
    let reservation = reserve_port_on(config.bind_address, config.app_port, config.strict_port)?;
    let mut prepared = prepare_run(config, reservation, store, routes, &operations, exposure)?;
    let mut running =
        match spawn_prepared_run(config, &mut prepared, None, None, &[], after_release) {
            Ok(running) => running,
            Err(error) => {
                exposure.withdraw(&operations, store, prepared.owner_id);
                abandon_prepared_run(config, &prepared, store, routes)?;
                return Err(error);
            }
        };
    running.signals = signals;
    match commit_prepared_runs(&[(config, &prepared, &running.child)], None, store, routes) {
        Ok(warnings) => {
            if !warnings.is_empty() {
                let warning = running.warning.get_or_insert_with(String::new);
                if !warning.is_empty() {
                    warning.push_str("; ");
                }
                warning.push_str(&warnings.join("; "));
            }
        }
        Err(error) => {
            let _ = running.child.signal(ProcessSignal::Kill);
            let _ = running.child.wait();
            exposure.withdraw(&operations, store, prepared.owner_id);
            abandon_prepared_run(config, &prepared, store, routes)?;
            return Err(error);
        }
    }
    Ok(running)
}

/// A reserved port and journaled route, with no child launched yet.
pub(crate) struct PreparedRun {
    reservation: Option<PortReservation>,
    pub(crate) owner_id: Uuid,
    operation_id: Uuid,
    target: String,
    displaced: Vec<RouteSpec>,
    pub(crate) exposed: Vec<(OsString, OsString)>,
    pub(crate) warnings: Vec<String>,
}

impl PreparedRun {
    pub(crate) fn release_port(&mut self) {
        self.reservation.take();
    }
}

pub(crate) fn prepare_run(
    config: &ResolvedRunConfig,
    reservation: PortReservation,
    store: &Store,
    routes: &mut impl RouteBackend,
    operations: &OperationGuard,
    exposure: &mut impl RunExposure,
) -> Result<PreparedRun, RunError> {
    let registry = store.load()?;
    if config.lan {
        let hostname = crate::lan::hostname(&config.hostname);
        if let Some(lease) = registry.leases.values().find(|lease| {
            lease.lan
                && lease.hostname != config.hostname
                && crate::lan::hostname(&lease.hostname) == hostname
        }) {
            return Err(RunError::LanHostnameConflict {
                hostname,
                project: lease.hostname.clone(),
            });
        }
    }
    let displaced = {
        let aliases = registry
            .aliases
            .values()
            .filter(|alias| alias.hostname == config.hostname)
            .map(route_for_alias);
        let leases = registry
            .leases
            .values()
            .filter(|lease| lease.hostname == config.hostname)
            .map(route_for_lease);
        aliases.chain(leases).collect::<Vec<_>>()
    };
    if !displaced.is_empty() && !config.force {
        return Err(RunError::Conflict(config.hostname.clone()));
    }
    let conflicts: Vec<_> = displaced
        .iter()
        .map(|route| (route.owner_id, route.tls))
        .collect();
    let port = reservation.port;
    let owner_id = Uuid::new_v4();
    let operation_id = Uuid::new_v4();
    let target = format!(
        "http://{}",
        SocketAddr::new(readiness_probe_address(config.bind_address), port)
    );
    let route = RouteSpec {
        lan: config.lan,
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
    if let Err(error) = routes.ensure(&route) {
        abandon_route(store, routes, config, owner_id, operation_id, &displaced)?;
        return Err(error.into());
    }
    let exposed = match exposure.expose(operations, store, owner_id, &config.hostname, &target) {
        Ok(exposed) => exposed,
        Err(error) => {
            abandon_route(store, routes, config, owner_id, operation_id, &displaced)?;
            return Err(error);
        }
    };
    let journal = store.mutate(|registry| {
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
    });
    if let Err(error) = journal {
        exposure.withdraw(operations, store, owner_id);
        abandon_route(store, routes, config, owner_id, operation_id, &displaced)?;
        return Err(error.into());
    }

    let mut warnings: Vec<String> = reservation.warning.iter().cloned().collect();
    if !conflicts.is_empty() {
        warnings.push(format!(
            "replaced existing Nook route for {}; the previous owner is no longer managed",
            config.hostname
        ));
    }
    Ok(PreparedRun {
        reservation: Some(reservation),
        owner_id,
        operation_id,
        target,
        displaced,
        exposed,
        warnings,
    })
}

pub(crate) fn spawn_prepared_run(
    config: &ResolvedRunConfig,
    prepared: &mut PreparedRun,
    directory: Option<&std::path::Path>,
    prefix: Option<&str>,
    extra_environment: &[(OsString, OsString)],
    after_release: impl FnOnce(u16),
) -> Result<RunningChild, RunError> {
    let signals = ForwardedSignals::new().map_err(Error::Spawn)?;
    let port = prepared
        .reservation
        .as_ref()
        .expect("prepared run is spawned once")
        .port;
    let owner_id = prepared.owner_id;
    let exposed = prepared.exposed.clone();
    let mut warnings = prepared.warnings.clone();
    let argv = substitute_port(&config.command, port);
    let framework = config.framework.resolve(&argv);
    let tailscale_hostname = exposed
        .iter()
        .find(|(name, _)| name == crate::tailscale::URL_VARIABLE)
        .and_then(|(_, value)| value.to_str())
        .and_then(|value| url::Url::parse(value).ok())
        .and_then(|url| url.host_str().map(str::to_owned));
    // Vite-based servers already allow *.localhost. Add the exposed hostname.
    let lan_hostname = config.lan.then(|| crate::lan::hostname(&config.hostname));
    let allowed_hostname = tailscale_hostname
        .as_deref()
        .or(lan_hostname.as_deref())
        .unwrap_or(&config.hostname);
    let argv = match framework {
        Some(framework) => framework.inject_argv(
            argv,
            port,
            config.bind_address,
            allowed_hostname,
            config.strict_port,
        ),
        None => argv,
    };
    let mut environment =
        child_environment(port, config.bind_address, &config.hostname, config.tls).to_vec();
    if let Some(framework) = framework {
        environment.extend(framework.environment(port, config.bind_address, allowed_hostname));
    }
    if let Some(hostname) = &lan_hostname {
        environment.push(("NOOK_LAN_URL".into(), format!("http://{hostname}").into()));
        let variable = "__VITE_ADDITIONAL_SERVER_ALLOWED_HOSTS";
        if let Some((_, value)) = environment.iter_mut().find(|(name, _)| name == variable) {
            if value != hostname.as_str() {
                value.push(",");
                value.push(hostname);
            }
        } else {
            let mut value = std::env::var_os(variable).unwrap_or_default();
            if !value.is_empty() {
                value.push(",");
            }
            value.push(hostname);
            environment.push((variable.into(), value));
        }
    }
    prepared
        .reservation
        .take()
        .expect("port is reserved until spawn")
        .release();
    after_release(port);
    let environment: Vec<_> = extra_environment
        .iter()
        .cloned()
        .chain(environment)
        .chain(exposed)
        .collect();
    let child = spawn_managed_child_in(&argv, &environment, owner_id, directory, prefix)?;
    let lan = if config.lan {
        match crate::lan::Advertisement::start(&config.hostname, 80, owner_id) {
            Ok(advertisement) => Some(advertisement),
            Err(error) => {
                warnings.push(format!("cannot announce the LAN hostname: {error}"));
                None
            }
        }
    } else {
        None
    };
    let warning = (!warnings.is_empty()).then(|| warnings.join("; "));
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
        lan,
    })
}

/// Persist ownership only once the caller's entire spawn phase succeeded.
pub(crate) fn commit_prepared_runs(
    runs: &[(&ResolvedRunConfig, &PreparedRun, &ManagedChild)],
    group_id: Option<Uuid>,
    store: &Store,
    routes: &mut impl RouteBackend,
) -> Result<Vec<String>, RunError> {
    let mut cleanup_failures = Vec::new();
    let mut warnings = Vec::new();
    for (config, prepared, _) in runs {
        for displaced in &prepared.displaced {
            if let Err(error) =
                routes.remove_if_owned(&config.hostname, displaced.owner_id, displaced.tls)
            {
                warnings.push(format!("cleanup of the previous route is pending: {error}"));
                cleanup_failures.push(PendingOperation {
                    id: Uuid::new_v4(),
                    kind: PendingOperationKind::RemoveRoute {
                        hostname: config.hostname.clone(),
                        owner_id: displaced.owner_id,
                        tls: displaced.tls,
                    },
                });
            }
        }
    }
    // One atomic registry mutation preserves displaced owners if any spawn fails.
    store.mutate(|registry| {
        for (config, prepared, child) in runs {
            let replaced_ids: std::collections::BTreeSet<_> = prepared
                .displaced
                .iter()
                .map(|route| route.owner_id)
                .collect();
            registry
                .aliases
                .retain(|_, alias| !replaced_ids.contains(&alias.id));
            registry.leases.retain(|id, _| !replaced_ids.contains(id));
            registry.pending_operations.retain(|operation| {
                operation.id != prepared.operation_id
                    && pending_owner(&operation.kind).is_none_or(|id| !replaced_ids.contains(&id))
            });
            registry.leases.insert(
                prepared.owner_id,
                Lease {
                    group_id,
                    workspace_supervisor: group_id.and_then(|id| {
                        registry
                            .workspace_groups
                            .get(&id)
                            .map(|group| group.supervisor.clone())
                    }),
                    lan: config.lan,
                    id: prepared.owner_id,
                    hostname: config.hostname.clone(),
                    target: prepared.target.clone(),
                    scheme: Scheme::Http,
                    tls: config.tls,
                    pid: child.pid,
                    pgid: child.pgid,
                    process_start_time_ticks: child.start_time_ticks,
                    state: LeaseState::Starting,
                },
            );
        }
        registry.pending_operations.extend(cleanup_failures);
        Ok(())
    })?;
    Ok(warnings)
}

pub(crate) fn abandon_prepared_run(
    config: &ResolvedRunConfig,
    prepared: &PreparedRun,
    store: &Store,
    routes: &mut impl RouteBackend,
) -> Result<(), RunError> {
    abandon_route(
        store,
        routes,
        config,
        prepared.owner_id,
        prepared.operation_id,
        &prepared.displaced,
    )
}

/// Removes the route of a run that will not start and drops its journal entry,
/// queueing the removal when Caddy cannot be reached. Restores displaced routes.
fn abandon_route(
    store: &Store,
    routes: &mut impl RouteBackend,
    config: &ResolvedRunConfig,
    owner_id: Uuid,
    operation_id: Uuid,
    displaced: &[RouteSpec],
) -> Result<(), RunError> {
    let cleanup = routes.remove_if_owned(&config.hostname, owner_id, config.tls);
    for route in displaced {
        // The registry still records these owners, so failed restorations are
        // retried by the next reconciliation.
        let _ = routes.ensure(&RouteSpec {
            replace_existing: true,
            ..route.clone()
        });
    }
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
    Ok(())
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
            if self.lan.as_mut().is_some_and(|lan| lan.is_displaced(store)) {
                self.lan.take();
            }
            if let Some(lan) = &self.lan {
                for warning in lan.warnings() {
                    warn(&warning);
                }
            }
            for signal in self.signals.pending() {
                self.child.signal_foreground(signal)?;
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
            #[cfg(unix)]
            self.child.sync_terminal();
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
        let exit_code = self.child.wait_with_signals(&mut self.signals, || {
            use std::io::Write;
            if self.lan.as_mut().is_some_and(|lan| lan.is_displaced(store)) {
                self.lan.take();
            }
            if let Some(lan) = &self.lan {
                for warning in lan.warnings() {
                    let _ = writeln!(io::stderr(), "warning: {warning}");
                }
            }
        })?;
        let warnings = self.cleanup(store, routes)?;
        Ok(RunOutcome {
            exit_code,
            warnings,
        })
    }

    pub(crate) fn cleanup(
        &mut self,
        store: &Store,
        routes: &mut impl RouteBackend,
    ) -> Result<Vec<String>, RunError> {
        self.lan.take();
        cleanup_run_route(&self.hostname, self.lease_id, self.tls, store, routes)
    }

    pub(crate) fn poll_readiness(&mut self, store: &Store) -> Result<bool, RunError> {
        if self.lan.as_mut().is_some_and(|lan| lan.is_displaced(store)) {
            self.lan.take();
        }
        if TcpStream::connect_timeout(
            &SocketAddr::new(readiness_probe_address(self.bind_address), self.port),
            Duration::from_millis(5),
        )
        .is_err()
        {
            return Ok(false);
        }
        store.mutate(|registry| {
            if let Some(lease) = registry.leases.get_mut(&self.lease_id) {
                lease.state = LeaseState::Ready;
            }
            Ok(())
        })?;
        Ok(true)
    }
}

/// Both direct children and runner members use the same ownership transaction.
pub(crate) fn cleanup_run_route(
    hostname: &str,
    owner_id: Uuid,
    tls: bool,
    store: &Store,
    routes: &mut impl RouteBackend,
) -> Result<Vec<String>, RunError> {
    let _operations = store.lock_operations()?;
    let cleanup = routes.remove_if_owned(hostname, owner_id, tls);
    let mut warnings = Vec::new();
    store.mutate(|registry| {
        registry.leases.remove(&owner_id);
        if let Err(error) = &cleanup {
            registry.pending_operations.push(PendingOperation {
                id: Uuid::new_v4(),
                kind: PendingOperationKind::RemoveRoute {
                    hostname: hostname.to_owned(),
                    owner_id,
                    tls,
                },
            });
            warnings.push(format!(
                "cleanup of {hostname} is pending: {error}; run `nook prune` to retry"
            ));
        }
        Ok(())
    })?;
    Ok(warnings)
}

pub(crate) fn readiness_probe_address(bind_address: IpAddr) -> IpAddr {
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
    PendingMember(String),
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
            Self::PendingMember(hostname) => write!(
                formatter,
                "workspace app `{hostname}` has not started; pending member association"
            ),
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
    let registry = store
        .load()
        .map_err(|error| StopError::State(error.to_string()))?;
    let lease = registry
        .leases
        .values()
        .find(|lease| lease.hostname == hostname)
        .ok_or_else(|| StopError::NotManaged(hostname.to_owned()))?;
    if let Some(group_id) = lease.group_id {
        let group = registry.workspace_groups.get(&group_id).ok_or_else(|| {
            StopError::State(format!("workspace group for `{hostname}` is missing"))
        })?;
        if lease.workspace_supervisor.as_ref() != Some(&group.supervisor) {
            return Err(StopError::State(format!(
                "workspace ownership for `{hostname}` does not match its group"
            )));
        }
        if let Some(runner) = &group.runner {
            if runner.id != group_id
                || runner.group_id != Some(group_id)
                || runner.workspace_supervisor.as_ref() != Some(&group.supervisor)
            {
                return Err(StopError::State(
                    "workspace runner identity does not match its group".into(),
                ));
            }
            // Prepared runner apps point to the runner until associate_member
            // replaces them. Never let an individual stop signal that tree,
            // including partial/mismatched identities in an inconsistent journal.
            if lease.pid == runner.pid || lease.pgid == runner.pgid {
                return Err(StopError::PendingMember(hostname.to_owned()));
            }
        }
    }
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
    spawn_managed_child_in(argv, environment, lease_id, None, None)
}

pub(crate) fn spawn_managed_child_in(
    argv: &[OsString],
    environment: &[(OsString, OsString)],
    lease_id: Uuid,
    directory: Option<&std::path::Path>,
    prefix: Option<&str>,
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
    } = prepare_windows_command(program, arguments, environment, lease_id, directory)?;
    let mut command = Command::new(program);
    // A run started from an exposed run must not inherit its tailnet URL;
    // an exposed child gets its own value back from `environment`.
    command
        .env_remove(crate::tailscale::URL_VARIABLE)
        .env_remove("NOOK_WORKSPACE_SESSION")
        .env_remove(crate::workspace_session::PROCESS_VARIABLE)
        .args(arguments)
        .envs(environment.iter().cloned());
    #[cfg(windows)]
    if let Some((key, value)) = internal_environment {
        command.env(key, value);
    }
    #[cfg(windows)]
    if let Some(raw_argument) = raw_argument {
        use std::os::windows::process::CommandExt;
        command.raw_arg(raw_argument);
    }
    if let Some(directory) = directory {
        command.current_dir(directory);
    }
    if prefix.is_some() {
        command
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
    } else {
        command
            .stdin(Stdio::inherit())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit());
    }
    // Session members keep the runner's inherited streams, but must never
    // take its foreground or propagate a background stdin stop to its group.
    // The root runner carries SESSION_VARIABLE; only members carry PROCESS_VARIABLE.
    let terminal_job_control = prefix.is_none()
        && !environment
            .iter()
            .any(|(key, _)| key == crate::workspace_session::PROCESS_VARIABLE);
    configure_child(&mut command, terminal_job_control);
    // Acquire before spawn: Command::spawn only returns after exec, so the
    // child claims the TTY in pre_exec. Nook keeps SIGTTOU ignored so later
    // wrapper writes survive `stty tostop`.
    #[cfg(unix)]
    let terminal = terminal_job_control
        .then(TerminalForeground::acquire)
        .flatten();
    let mut child = command.spawn().map_err(Error::Spawn)?;
    let pid = child.id();
    #[cfg(unix)]
    join_child_process_group(pid);
    let Some(identity) = read_process_identity(pid) else {
        cleanup_unidentified_child(&mut child, pid);
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
    let mut output_threads = Vec::new();
    if let Some(prefix) = prefix {
        if let Some(stdout) = child.stdout.take() {
            output_threads.push(prefix_output(stdout, prefix.to_owned(), false));
        }
        if let Some(stderr) = child.stderr.take() {
            output_threads.push(prefix_output(stderr, prefix.to_owned(), true));
        }
    }
    #[cfg(unix)]
    if let Some(terminal) = &terminal {
        terminal.handoff(identity.pgid);
    }
    Ok(ManagedChild {
        child,
        output_threads,
        pid,
        pgid: identity.pgid,
        start_time_ticks: identity.start_time_ticks,
        #[cfg(unix)]
        workspace_owned: environment.iter().any(|(key, _)| {
            key == crate::workspace_session::SESSION_VARIABLE
                || key == crate::workspace_session::PROCESS_VARIABLE
        }),
        #[cfg(unix)]
        terminal,
        #[cfg(unix)]
        terminal_job_control,
        #[cfg(windows)]
        job,
    })
}

fn prefix_output(
    reader: impl io::Read + Send + 'static,
    prefix: String,
    stderr: bool,
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        use io::{BufRead, Read, Write};
        // Bound memory even when a child writes an unbroken stream.
        let mut reader = io::BufReader::new(reader);
        loop {
            let mut line = Vec::new();
            match reader.by_ref().take(8192).read_until(b'\n', &mut line) {
                Ok(0) | Err(_) => break,
                Ok(_) => {
                    let mut output: Box<dyn Write> = if stderr {
                        Box::new(io::stderr().lock())
                    } else {
                        Box::new(io::stdout().lock())
                    };
                    let _ = write!(output, "[{prefix}] ");
                    let _ = output.write_all(&line);
                    if !line.ends_with(b"\n") {
                        let _ = output.write_all(b"\n");
                    }
                    let _ = output.flush();
                }
            }
        }
    })
}

fn cleanup_unidentified_child(child: &mut Child, _pid: u32) {
    #[cfg(unix)]
    {
        // configure_child creates a process group whose id is the child's pid.
        // The leader may already be gone while descendants still occupy that
        // group, so killing only the Child handle would leave them unmanaged.
        if i32::try_from(_pid)
            .ok()
            .and_then(|pgid| send_group_signal(pgid, libc::SIGKILL).ok())
            .is_none()
        {
            let _ = child.kill();
        }
    }
    #[cfg(windows)]
    let _ = child.kill();
    let _ = child.wait();
}

#[cfg(unix)]
struct TerminalForeground {
    previous_pgid: libc::pid_t,
    previous_ttou: libc::sighandler_t,
}

#[cfg(unix)]
#[allow(unsafe_code)]
impl TerminalForeground {
    fn acquire() -> Option<Self> {
        // SAFETY: stdin is the caller's inherited terminal, if any. Claiming
        // only when this process group already owns the foreground avoids
        // stealing the TTY from the shell after `bg`. SIGTTOU stays ignored
        // for the rest of the handoff so wrapper writes survive `stty tostop`.
        unsafe {
            if libc::isatty(libc::STDIN_FILENO) == 0 {
                return None;
            }
            let previous_pgid = libc::tcgetpgrp(libc::STDIN_FILENO);
            if !owns_foreground_terminal(previous_pgid, libc::getpgrp()) {
                return None;
            }
            let previous_ttou = libc::signal(libc::SIGTTOU, libc::SIG_IGN);
            Some(Self {
                previous_pgid,
                previous_ttou,
            })
        }
    }

    fn handoff(&self, pgid: i32) {
        // pre_exec already claimed the TTY. Repeating after spawn would steal
        // it from a descendant if the child (bash -m) already moved it.
        unsafe {
            let current = libc::tcgetpgrp(libc::STDIN_FILENO);
            if !parent_may_reassign_foreground(current, self.previous_pgid, pgid) {
                return;
            }
            let _ = libc::tcsetpgrp(libc::STDIN_FILENO, pgid);
        }
    }

    fn claim(pgid: i32) -> Option<Self> {
        let terminal = Self::acquire()?;
        terminal.handoff(pgid);
        Some(terminal)
    }

    fn restore(&self) {
        unsafe {
            let _ = libc::tcsetpgrp(libc::STDIN_FILENO, self.previous_pgid);
            libc::signal(libc::SIGTTOU, self.previous_ttou);
        }
    }
}

#[cfg(unix)]
impl Drop for TerminalForeground {
    fn drop(&mut self) {
        self.restore();
    }
}

#[cfg(unix)]
fn owns_foreground_terminal(foreground_pgid: libc::pid_t, our_pgid: libc::pid_t) -> bool {
    foreground_pgid >= 0 && foreground_pgid == our_pgid
}

#[cfg(unix)]
fn parent_may_reassign_foreground(
    current: libc::pid_t,
    previous: libc::pid_t,
    child: libc::pid_t,
) -> bool {
    current == previous || current == child
}

#[cfg(unix)]
fn process_is_stopped_state(state: u8) -> bool {
    state == b'T'
}

#[cfg(unix)]
fn process_is_stopped(pid: u32) -> bool {
    read_process_identity(pid).is_some_and(|identity| process_is_stopped_state(identity.state))
}

#[cfg(unix)]
#[allow(unsafe_code)]
fn join_child_process_group(pid: u32) {
    let Ok(pid) = i32::try_from(pid) else {
        return;
    };
    // SAFETY: pid is the child we just spawned in this session. Fails with
    // EACCES once the child has exec'd; that race is expected and ignored.
    unsafe {
        libc::setpgid(pid, pid);
    }
}

#[cfg(unix)]
#[allow(unsafe_code)]
fn configure_child(command: &mut Command, terminal_job_control: bool) {
    // SAFETY: this closure runs after fork and before exec. It only invokes
    // async-signal-safe libc syscalls and constructs errors from errno.
    // The child claims the TTY here because Command::spawn returns only after
    // exec; permanently ignoring SIGTTIN would turn a too-early stdin read
    // into EIO instead of blocking until the handoff.
    unsafe {
        command.pre_exec(move || {
            if libc::setpgid(0, 0) == -1 {
                return Err(io::Error::last_os_error());
            }
            if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM) == -1 {
                return Err(io::Error::last_os_error());
            }
            if libc::getppid() == 1 {
                libc::raise(libc::SIGTERM);
            }
            if terminal_job_control {
                claim_controlling_terminal();
            }
            Ok(())
        });
    }
}

#[cfg(unix)]
#[allow(unsafe_code)]
fn claim_controlling_terminal() {
    // SAFETY: pre_exec; only async-signal-safe syscalls. Take the foreground
    // when the parent still owns it, then restore default job-control
    // dispositions so the exec'd program can stop on SIGTSTP/SIGTTIN.
    unsafe {
        if libc::isatty(libc::STDIN_FILENO) == 0 {
            return;
        }
        let foreground = libc::tcgetpgrp(libc::STDIN_FILENO);
        let parent_pgid = libc::getpgid(libc::getppid());
        if parent_pgid < 0 || foreground != parent_pgid {
            return;
        }
        libc::signal(libc::SIGTTOU, libc::SIG_IGN);
        let _ = libc::tcsetpgrp(libc::STDIN_FILENO, libc::getpgrp());
        libc::signal(libc::SIGTTOU, libc::SIG_DFL);
        libc::signal(libc::SIGTTIN, libc::SIG_DFL);
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
    directory: Option<&std::path::Path>,
) -> Result<PreparedWindowsCommand, Error> {
    let Some(resolved) = resolve_windows_program(&program, environment, directory) else {
        return Ok(PreparedWindowsCommand {
            program,
            arguments,
            raw_argument: None,
            internal_environment: None,
        });
    };
    let is_batch = resolved
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            extension.eq_ignore_ascii_case("cmd") || extension.eq_ignore_ascii_case("bat")
        });
    if !is_batch {
        return Ok(PreparedWindowsCommand {
            program: resolved.into_os_string(),
            arguments,
            raw_argument: None,
            internal_environment: None,
        });
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
        windows_batch_command_line(resolved.as_os_str(), &arguments, &percent_variable)
            .map_err(Error::Spawn)?;
    Ok(PreparedWindowsCommand {
        program: interpreter,
        arguments: shell_arguments,
        raw_argument: Some(command_line),
        internal_environment: Some((OsString::from(percent_variable), OsString::from("%"))),
    })
}

#[cfg(windows)]
fn windows_batch_command_line(
    program: &std::ffi::OsStr,
    arguments: &[OsString],
    percent_variable: &str,
) -> io::Result<OsString> {
    use std::os::windows::ffi::{OsStrExt, OsStringExt};

    let mut command_line = vec![b'"' as u16];
    let percent_reference = format!("%{percent_variable}%")
        .encode_utf16()
        .collect::<Vec<_>>();
    for (index, argument) in std::iter::once(program)
        .chain(arguments.iter().map(OsString::as_os_str))
        .enumerate()
    {
        if argument
            .encode_wide()
            .any(|unit| unit == b'\r' as u16 || unit == b'\n' as u16)
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "batch command arguments cannot contain line breaks",
            ));
        }
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
    Ok(OsString::from_wide(&command_line))
}

#[cfg(windows)]
fn resolve_windows_program(
    program: &std::ffi::OsStr,
    environment: &[(OsString, OsString)],
    directory: Option<&std::path::Path>,
) -> Option<std::path::PathBuf> {
    use std::path::{Path, PathBuf};

    let program = Path::new(program);
    let current_directory = match directory {
        Some(directory) => directory.to_path_buf(),
        None => env::current_dir().ok()?,
    };
    let has_directory = program.is_absolute()
        || program
            .parent()
            .is_some_and(|parent| !parent.as_os_str().is_empty());
    let directories = if has_directory {
        vec![current_directory.clone()]
    } else {
        let mut directories = vec![current_directory.clone()];
        if let Some(path) = effective_windows_env(environment, "PATH") {
            directories.extend(env::split_paths(&path).map(|entry| current_directory.join(entry)));
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
fn configure_child(command: &mut Command, _terminal_job_control: bool) {
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
fn signal_managed_child(child: &ManagedChild, signal: ProcessSignal) -> io::Result<bool> {
    // A reaped leader's numeric PGID may already belong to another process.
    // Runner recovery separately finds marked descendants when this is absent.
    if child.workspace_owned
        && !read_process_identity(child.pid).is_some_and(|identity| {
            identity.pgid == child.pgid && identity.start_time_ticks == child.start_time_ticks
        })
    {
        return Ok(false);
    }
    let signal = match signal {
        ProcessSignal::Interrupt => libc::SIGINT,
        ProcessSignal::Terminate => libc::SIGTERM,
        ProcessSignal::Kill => libc::SIGKILL,
    };
    send_group_signal(child.pgid, signal).map(|()| true)
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
pub(crate) fn read_process_identity(pid: u32) -> Option<ProcIdentity> {
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
fn signal_with_forced_fallback(
    signal: ProcessSignal,
    mut send: impl FnMut(ProcessSignal) -> io::Result<()>,
) -> io::Result<()> {
    match send(signal) {
        Err(_) if signal != ProcessSignal::Kill => send(ProcessSignal::Kill),
        result => result,
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
pub(crate) fn read_process_identity(pid: u32) -> Option<ProcIdentity> {
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

#[cfg(unix)]
impl Drop for ManagedChild {
    fn drop(&mut self) {
        drop(self.terminal.take());
    }
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

pub(crate) fn substitute_values(
    argument: &std::ffi::OsStr,
    values: &std::collections::BTreeMap<String, String>,
) -> OsString {
    #[cfg(unix)]
    {
        let mut output = argument.as_bytes().to_vec();
        for (key, value) in values {
            output = replace_bytes(&output, format!("${{{key}}}").as_bytes(), value.as_bytes());
        }
        OsString::from_vec(output)
    }
    #[cfg(windows)]
    {
        use std::os::windows::ffi::{OsStrExt, OsStringExt};
        let mut output: Vec<_> = argument.encode_wide().collect();
        for (key, value) in values {
            output = replace_wide(
                &output,
                &format!("${{{key}}}").encode_utf16().collect::<Vec<_>>(),
                &value.encode_utf16().collect::<Vec<_>>(),
            );
        }
        OsString::from_wide(&output)
    }
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

/// The workspace runner may share its caller's process group. Its ownership
/// depends only on PID/start time, and never authorizes signaling that group.
#[cfg(unix)]
pub(crate) fn workspace_supervisor_liveness(supervisor: &WorkspaceSupervisor) -> Liveness {
    match fs::read_to_string(format!("/proc/{}/stat", supervisor.pid)) {
        Ok(stat) => match parse_stat(&stat) {
            Some(identity) => {
                identity_liveness(identity, identity.pgid, supervisor.process_start_time_ticks)
            }
            None => Liveness::Indeterminate,
        },
        Err(error) if error.kind() == io::ErrorKind::NotFound => Liveness::Dead,
        Err(_) => Liveness::Indeterminate,
    }
}

#[cfg(windows)]
pub(crate) fn lease_liveness(lease: &Lease) -> Liveness {
    process_liveness(lease.pid, lease.process_start_time_ticks)
}

#[cfg(windows)]
pub(crate) fn workspace_supervisor_liveness(supervisor: &WorkspaceSupervisor) -> Liveness {
    process_liveness(supervisor.pid, supervisor.process_start_time_ticks)
}

#[cfg(windows)]
#[allow(unsafe_code)]
fn process_liveness(pid: u32, start_time_ticks: u64) -> Liveness {
    use windows_sys::Win32::Foundation::{CloseHandle, WAIT_TIMEOUT};
    use windows_sys::Win32::Storage::FileSystem::SYNCHRONIZE;
    use windows_sys::Win32::System::Threading::{
        OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, WaitForSingleObject,
    };

    let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION | SYNCHRONIZE, 0, pid) };
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
    match read_process_identity(pid) {
        Some(identity) if identity.start_time_ticks == start_time_ticks => Liveness::Alive,
        Some(_) => Liveness::Dead,
        None => Liveness::Indeterminate,
    }
}

/// Kill only a currently verified managed application group/job. A matching
/// Unix zombie still proves group ownership; missing/reused PIDs do not.
pub(crate) fn kill_owned_lease(lease: &Lease) -> Result<(), Error> {
    signal_owned_lease(lease, ProcessSignal::Kill).map(|_| ())
}

/// Signal only a verified independent application group, including a zombie
/// leader that still proves ownership. All signal kinds share the same guards.
/// Returns true only when delivered; missing/reused identities are a false no-op.
pub(crate) fn signal_owned_lease(lease: &Lease, signal: ProcessSignal) -> Result<bool, Error> {
    match lease_liveness(lease) {
        Liveness::Dead => {
            #[cfg(unix)]
            if !read_process_identity(lease.pid).is_some_and(|identity| {
                matches!(identity.state, b'Z' | b'X')
                    && identity.pgid == lease.pgid
                    && identity.start_time_ticks == lease.process_start_time_ticks
            }) {
                return Ok(false);
            }
            #[cfg(windows)]
            return Ok(false);
        }
        Liveness::Indeterminate => return Err(Error::ProcessIdentity(lease.pid)),
        Liveness::Alive => {}
    }
    if lease.pid == std::process::id()
        || lease
            .workspace_supervisor
            .as_ref()
            .is_some_and(|supervisor| supervisor.pid == lease.pid)
    {
        return Err(Error::Spawn(io::Error::other(
            "refusing to kill the workspace supervisor or caller",
        )));
    }
    #[cfg(unix)]
    {
        // Managed children lead their own group. Never signal the caller's or
        // workspace supervisor's group, even if a malformed lease names it.
        if lease.pgid <= 1
            || i32::try_from(lease.pid) != Ok(lease.pgid)
            || read_process_identity(std::process::id())
                .is_none_or(|identity| identity.pgid == lease.pgid)
            || lease
                .workspace_supervisor
                .as_ref()
                .is_some_and(|supervisor| {
                    read_process_identity(supervisor.pid).is_some_and(|identity| {
                        identity.start_time_ticks == supervisor.process_start_time_ticks
                            && identity.pgid == lease.pgid
                    })
                })
        {
            return Err(Error::Spawn(io::Error::other(
                "refusing to kill a lease outside its own application process group",
            )));
        }
    }
    #[cfg(unix)]
    {
        owned_group_signal_result(signal_lease(lease, signal))
    }
    #[cfg(windows)]
    {
        match signal {
            ProcessSignal::Kill => kill_owned_job(lease),
            _ => signal_lease(lease, signal),
        }
        .map(|()| true)
        .map_err(Error::Spawn)
    }
}

#[cfg(unix)]
fn owned_group_signal_result(result: io::Result<()>) -> Result<bool, Error> {
    match result {
        Ok(()) => Ok(true),
        // The verified group can exit and be reaped before kill(2). No signal
        // was delivered, so marker recovery must still inspect descendants.
        Err(error) if error.raw_os_error() == Some(libc::ESRCH) => Ok(false),
        Err(error) => Err(Error::Spawn(error)),
    }
}

/// Recover the independent runner tree even after its leader has been reaped.
/// No environment is persisted or reported; only an exact inherited marker is
/// compared in memory. pidfds pin each verified process across PID reuse.
pub(crate) fn cleanup_workspace_processes(
    group_id: Uuid,
    group: &crate::state::WorkspaceGroup,
) -> io::Result<()> {
    if let Some(runner) = &group.runner {
        if runner.id != group_id
            || runner.group_id != Some(group_id)
            || runner.workspace_supervisor.as_ref() != Some(&group.supervisor)
        {
            return Err(io::Error::other(
                "workspace runner identity does not match its group",
            ));
        }
        kill_owned_lease(runner).map_err(io::Error::other)?;
    }
    cleanup_marked_workspace_processes(
        crate::workspace_session::recovery_path(group_id, group)?,
        None,
        group.supervisor.process_start_time_ticks,
    )
}

pub(crate) fn cleanup_workspace_member_processes(
    path: &std::path::Path,
    token: Uuid,
    started_after: u64,
    child: Option<&Lease>,
) -> io::Result<()> {
    if let Some(child) = child {
        kill_owned_lease(child).map_err(io::Error::other)?;
    }
    cleanup_marked_workspace_processes(path, Some(token), started_after)
}

#[cfg(target_os = "linux")]
struct RecoveryMarkers {
    exact: Vec<Vec<u8>>,
    claim_prefix: Option<Vec<u8>>,
}

#[cfg(target_os = "linux")]
impl RecoveryMarkers {
    fn new(path: &std::path::Path, claim: Option<Uuid>) -> Self {
        let marker = |key: &str, value: &std::ffi::OsStr| {
            let mut marker = key.as_bytes().to_vec();
            marker.push(b'=');
            marker.extend_from_slice(value.as_bytes());
            marker
        };
        if let Some(token) = claim {
            Self {
                exact: vec![marker(
                    crate::workspace_session::PROCESS_VARIABLE,
                    &crate::workspace_session::member_process_marker(path, token),
                )],
                claim_prefix: None,
            }
        } else {
            let process = marker(crate::workspace_session::PROCESS_VARIABLE, path.as_os_str());
            let mut prefix = process.clone();
            prefix.push(b'/');
            Self {
                exact: vec![
                    marker(crate::workspace_session::SESSION_VARIABLE, path.as_os_str()),
                    process,
                ],
                claim_prefix: Some(prefix),
            }
        }
    }

    fn matches(&self, environment: &[u8]) -> bool {
        environment.split(|byte| *byte == 0).any(|value| {
            self.exact.iter().any(|marker| value == marker)
                || self.claim_prefix.as_ref().is_some_and(|prefix| {
                    value
                        .strip_prefix(prefix.as_slice())
                        .and_then(|token| std::str::from_utf8(token).ok())
                        .is_some_and(|token| Uuid::parse_str(token).is_ok())
                })
        })
    }
}

#[cfg(target_os = "linux")]
fn cleanup_marked_workspace_processes(
    path: &std::path::Path,
    claim: Option<Uuid>,
    started_after: u64,
) -> io::Result<()> {
    let deadline = Instant::now() + Duration::from_secs(2);
    while signal_marked_workspace_processes(path, claim, started_after, ProcessSignal::Kill, &[])? {
        if Instant::now() >= deadline {
            return Err(io::Error::other(
                "workspace descendants have not exited; retry prune",
            ));
        }
        thread::sleep(Duration::from_millis(10));
    }
    Ok(())
}

/// One pass over exactly marked descendants. Graceful shutdown uses the same
/// identity, namespace and pidfd protections as forced recovery. Skip groups
/// only after a verified group signal was actually delivered to them.
#[cfg(target_os = "linux")]
pub(crate) fn signal_marked_workspace_processes(
    path: &std::path::Path,
    claim: Option<Uuid>,
    started_after: u64,
    signal: ProcessSignal,
    signaled_groups: &[i32],
) -> io::Result<bool> {
    use std::os::unix::fs::MetadataExt;
    let markers = RecoveryMarkers::new(path, claim);
    let uid = fs::metadata("/proc/self")?.uid();
    let namespace = fs::read_link("/proc/self/ns/user")?;
    let uid_map = fs::read("/proc/self/uid_map")?;
    let mut found = false;
    for entry in fs::read_dir("/proc")? {
        let entry = entry?;
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|name| name.parse::<u32>().ok())
        else {
            continue;
        };
        if pid == std::process::id() {
            continue;
        }
        // An inaccessible candidate is not proof of workspace ownership.
        // Establish the exact marker before treating inspection failures as
        // recovery errors, so unrelated protected processes cannot block it.
        let Ok(environment) = fs::read(entry.path().join("environ")) else {
            continue;
        };
        if !markers.matches(&environment) {
            continue;
        }
        let metadata = match entry.metadata() {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error),
        };
        if metadata.uid() != uid {
            continue;
        }
        let Some(identity) = read_process_identity(pid) else {
            if entry.path().exists() {
                return Err(io::Error::other(
                    "cannot verify workspace candidate identity",
                ));
            }
            continue;
        };
        if matches!(identity.state, b'Z' | b'X') || signaled_groups.contains(&identity.pgid) {
            continue;
        }
        // Exact inherited ownership also covers setsid descendants and
        // processes launched before their lease association was persisted.
        if identity.start_time_ticks < started_after {
            continue;
        }
        // Host processes mapped into a test's user namespace are not
        // candidates, even when their displayed uid happens to match.
        match fs::read(entry.path().join("uid_map")) {
            Ok(candidate_map) if candidate_map != uid_map => continue,
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error),
            Ok(_) => {}
        }
        if !workspace_namespace_matches(
            pid,
            identity,
            &namespace,
            fs::read_link(entry.path().join("ns/user")),
        )? {
            continue;
        }
        found |= signal_marked_process(pid, identity, &markers, signal)?;
    }
    Ok(found)
}

#[cfg(target_os = "linux")]
fn workspace_namespace_matches(
    pid: u32,
    expected: ProcIdentity,
    namespace: &std::path::Path,
    result: io::Result<std::path::PathBuf>,
) -> io::Result<bool> {
    match result {
        Ok(candidate) => Ok(candidate == namespace),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) if error.kind() == io::ErrorKind::PermissionDenied => {
            // proc_ns_readlink also returns EACCES if the task disappears
            // after resolving its namespace dentry. Only confirmed exit or
            // PID reuse makes this harmless; unknown/live identity keeps the
            // original permission error and leaves recovery pending.
            if workspace_process_exited(pid, expected) {
                Ok(false)
            } else {
                Err(error)
            }
        }
        Err(error) => Err(error),
    }
}

#[cfg(target_os = "linux")]
fn workspace_process_exited(pid: u32, expected: ProcIdentity) -> bool {
    match fs::read_to_string(format!("/proc/{pid}/stat")) {
        Ok(stat) => parse_stat(&stat).is_some_and(|identity| {
            matches!(identity.state, b'Z' | b'X')
                || identity.start_time_ticks != expected.start_time_ticks
        }),
        Err(error) if error.kind() == io::ErrorKind::NotFound => true,
        _ => false,
    }
}

#[cfg(target_os = "linux")]
#[allow(unsafe_code)]
fn signal_marked_process(
    pid: u32,
    expected: ProcIdentity,
    markers: &RecoveryMarkers,
    signal: ProcessSignal,
) -> io::Result<bool> {
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
    // Open before re-reading /proc. Even if the numeric PID is reused during
    // validation, the signal can only reach the process pinned by this fd.
    let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, pid, 0) };
    if fd < 0 {
        let error = io::Error::last_os_error();
        return if error.raw_os_error() == Some(libc::ESRCH) {
            Ok(false)
        } else {
            Err(error)
        };
    }
    let fd = unsafe { OwnedFd::from_raw_fd(fd as i32) };
    if !read_process_identity(pid).is_some_and(|identity| {
        !matches!(identity.state, b'Z' | b'X')
            && identity.pgid == expected.pgid
            && identity.start_time_ticks == expected.start_time_ticks
    }) {
        return Ok(false);
    }
    if !workspace_environment_matches(
        pid,
        expected,
        markers,
        fs::read(format!("/proc/{pid}/environ")),
    )? {
        return Ok(false);
    }
    let signal = match signal {
        ProcessSignal::Interrupt => libc::SIGINT,
        ProcessSignal::Terminate => libc::SIGTERM,
        ProcessSignal::Kill => libc::SIGKILL,
    };
    let result = unsafe {
        libc::syscall(
            libc::SYS_pidfd_send_signal,
            fd.as_raw_fd(),
            signal,
            std::ptr::null::<libc::siginfo_t>(),
            0,
        )
    };
    if result < 0 && io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH) {
        return Err(io::Error::last_os_error());
    }
    Ok(true)
}

#[cfg(target_os = "linux")]
fn workspace_environment_matches(
    pid: u32,
    expected: ProcIdentity,
    markers: &RecoveryMarkers,
    result: io::Result<Vec<u8>>,
) -> io::Result<bool> {
    match result {
        Ok(environment) => Ok(markers.matches(&environment)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) if error.raw_os_error() == Some(libc::ESRCH) => Ok(false),
        // The task can exit after the pidfd identity check. Its /proc files
        // can then deny access; only fresh exit/reuse evidence makes this
        // harmless. A protected live or unknown owner keeps its raw error.
        Err(error)
            if error.kind() == io::ErrorKind::PermissionDenied
                && workspace_process_exited(pid, expected) =>
        {
            Ok(false)
        }
        Err(error) => Err(error),
    }
}

#[cfg(windows)]
fn cleanup_marked_workspace_processes(
    _path: &std::path::Path,
    _claim: Option<Uuid>,
    _started_after: u64,
) -> io::Result<()> {
    // Every runner is assigned to a non-inherited KILL_ON_JOB_CLOSE job before
    // it is resumed. Root death closes the last handle, including the window
    // before its process identity could be persisted in the registry.
    Ok(())
}

#[cfg(all(unix, not(target_os = "linux")))]
fn cleanup_marked_workspace_processes(
    _path: &std::path::Path,
    _claim: Option<Uuid>,
    _started_after: u64,
) -> io::Result<()> {
    Err(io::Error::other(
        "workspace descendant recovery requires Linux /proc",
    ))
}

#[cfg(windows)]
#[allow(unsafe_code)]
fn kill_owned_job(lease: &Lease) -> io::Result<()> {
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::JobObjects::{
        IsProcessInJob, OpenJobObjectW, TerminateJobObject,
    };
    use windows_sys::Win32::System::SystemServices::{JOB_OBJECT_QUERY, JOB_OBJECT_TERMINATE};
    use windows_sys::Win32::System::Threading::{OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION};

    let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, lease.pid) };
    if process.is_null() {
        return Err(io::Error::last_os_error());
    }
    let name = job_name(lease.id);
    let job = unsafe { OpenJobObjectW(JOB_OBJECT_QUERY | JOB_OBJECT_TERMINATE, 0, name.as_ptr()) };
    if job.is_null() {
        let error = io::Error::last_os_error();
        unsafe { CloseHandle(process) };
        return Err(error);
    }
    let mut belongs = 0;
    let result = if unsafe { IsProcessInJob(process, job, &mut belongs) } == 0 {
        Err(io::Error::last_os_error())
    } else if belongs == 0 || lease_liveness(lease) != Liveness::Alive {
        Err(io::Error::other(
            "cannot verify application ownership of the named job",
        ))
    } else if unsafe { TerminateJobObject(job, 1) } == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    };
    unsafe {
        CloseHandle(job);
        CloseHandle(process);
    }
    result
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
    use std::process::Command;
    use std::thread;
    use std::time::{Duration, Instant};

    use super::{
        Error, Liveness, LocalOnly, ProcIdentity, ProcessSignal, RunError, RunExposure, StopError,
        StopSystem, child_environment, cleanup_unidentified_child, configure_child,
        identity_liveness, owns_foreground_terminal, parent_may_reassign_foreground, parse_stat,
        process_is_stopped, process_is_stopped_state, read_process_identity,
        readiness_probe_address, reserve_port, send_group_signal, spawn_child, start_exposed_run,
        start_run, start_run_with_hook, stop_managed, substitute_port,
    };
    use crate::config::ResolvedRunConfig;
    use crate::reconcile::{RouteBackend, RouteError, RouteSpec};
    use crate::state::{
        Alias, LeaseState, OperationGuard, PendingOperationKind, Scheme, Store, decode,
    };
    use uuid::Uuid;

    #[derive(Default)]
    struct RecordingExposure {
        fail: bool,
        exposed_after_route_before_lease: Vec<Uuid>,
        withdrawn: Vec<Uuid>,
    }

    impl RunExposure for RecordingExposure {
        fn expose(
            &mut self,
            _operations: &OperationGuard,
            store: &Store,
            owner_id: Uuid,
            _hostname: &str,
            _target: &str,
        ) -> Result<Vec<(OsString, OsString)>, RunError> {
            let registry = store.load().unwrap();
            let route_journaled = registry.pending_operations.iter().any(|operation| {
                matches!(
                    operation.kind,
                    PendingOperationKind::InstallRoute { owner_id: id, .. }
                        | PendingOperationKind::StartProcess { owner_id: id, .. } if id == owner_id
                )
            });
            if route_journaled && !registry.leases.contains_key(&owner_id) {
                self.exposed_after_route_before_lease.push(owner_id);
            }
            if self.fail {
                return Err(RunError::Exposure("tailnet unavailable".into()));
            }
            Ok(vec![(
                OsString::from("NOOK_TAILSCALE_URL"),
                OsString::from("https://host.example.ts.net:8443"),
            )])
        }

        fn withdraw(&mut self, _operations: &OperationGuard, _store: &Store, owner_id: Uuid) {
            self.withdrawn.push(owner_id);
        }
    }

    #[test]
    fn exposure_runs_between_route_and_spawn_and_its_variables_reach_the_child() {
        let (store, path) = temporary_store();
        let marker = path.parent().unwrap().join("tailscale-url");
        let script = format!("printf %s \"$NOOK_TAILSCALE_URL\" > '{}'", marker.display());
        let config = run_config(vec!["/bin/sh", "-c", &script], 30);
        let mut routes = Routes::default();
        let mut exposure = RecordingExposure::default();
        let mut running = start_exposed_run(&config, &store, &mut routes, &mut exposure).unwrap();
        assert_eq!(
            exposure.exposed_after_route_before_lease,
            [running.lease_id]
        );
        assert_eq!(running.finish(&store, &mut routes).unwrap().exit_code, 0);
        assert_eq!(
            std::fs::read_to_string(&marker).unwrap(),
            "https://host.example.ts.net:8443"
        );
        assert!(exposure.withdrawn.is_empty());
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn failed_exposure_never_spawns_and_removes_the_route() {
        let (store, path) = temporary_store();
        let marker = path.parent().unwrap().join("child-started");
        let script = format!("touch '{}'", marker.display());
        let config = run_config(vec!["/bin/sh", "-c", &script], 30);
        let mut routes = Routes::default();
        let mut exposure = RecordingExposure {
            fail: true,
            ..RecordingExposure::default()
        };
        assert!(matches!(
            start_exposed_run(&config, &store, &mut routes, &mut exposure),
            Err(RunError::Exposure(_))
        ));
        thread::sleep(Duration::from_millis(100));
        assert!(!marker.exists());
        assert!(routes.owners.is_empty());
        let registry = decode(&std::fs::read(&path).unwrap()).unwrap();
        assert!(registry.leases.is_empty());
        assert!(registry.pending_operations.is_empty());
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn failed_exposure_of_a_forced_run_restores_the_displaced_alias() {
        let (store, path) = temporary_store();
        let mut config = run_config(vec!["/bin/true"], 30);
        config.force = true;
        let displaced = Alias {
            id: Uuid::new_v4(),
            hostname: config.hostname.clone(),
            target: "http://127.0.0.1:3000/".into(),
            scheme: Scheme::Http,
            tls: true,
            preserve_host: true,
        };
        store
            .mutate(|registry| {
                registry
                    .aliases
                    .insert(displaced.hostname.clone(), displaced.clone());
                Ok(())
            })
            .unwrap();
        let mut routes = Routes {
            owners: BTreeMap::from([(displaced.hostname.clone(), displaced.id)]),
            ..Routes::default()
        };
        let mut exposure = RecordingExposure {
            fail: true,
            ..RecordingExposure::default()
        };
        assert!(matches!(
            start_exposed_run(&config, &store, &mut routes, &mut exposure),
            Err(RunError::Exposure(_))
        ));
        assert_eq!(exposure.exposed_after_route_before_lease.len(), 1);
        assert_eq!(routes.owners[&displaced.hostname], displaced.id);
        let registry = decode(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(
            registry.aliases.into_values().collect::<Vec<_>>(),
            [displaced]
        );
        assert!(registry.leases.is_empty());
        assert!(registry.pending_operations.is_empty());
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn failed_spawn_withdraws_the_exposure_it_created() {
        let (store, path) = temporary_store();
        let config = run_config(vec!["/nonexistent/nook-test-command"], 30);
        let mut routes = Routes::default();
        let mut exposure = RecordingExposure::default();
        assert!(matches!(
            start_exposed_run(&config, &store, &mut routes, &mut exposure),
            Err(RunError::Process(Error::Spawn(_)))
        ));
        assert_eq!(
            exposure.withdrawn,
            exposure.exposed_after_route_before_lease
        );
        assert_eq!(exposure.withdrawn.len(), 1);
        assert!(routes.owners.is_empty());
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

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
    fn unidentified_exited_leader_does_not_leave_its_worker_running() {
        let directory = std::env::temp_dir().join(format!("nook-orphan-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&directory).unwrap();
        let marker = directory.join("worker.pid");
        let mut command = Command::new("/usr/bin/python3");
        command
            .args([
                "-c",
                "import os,subprocess; p=subprocess.Popen(['/bin/sleep','30']); open(os.environ['NOOK_WORKER_PID'],'w').write(str(p.pid))",
            ])
            .env("NOOK_WORKER_PID", &marker);
        configure_child(&mut command, true);
        let mut leader = command.spawn().unwrap();
        let leader_pid = leader.id();
        assert!(leader.wait().unwrap().success());
        let worker_pid: u32 = std::fs::read_to_string(&marker).unwrap().parse().unwrap();
        let worker_was_alive = read_process_identity(worker_pid)
            .is_some_and(|identity| !matches!(identity.state, b'Z' | b'X'));

        cleanup_unidentified_child(&mut leader, leader_pid);

        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline
            && read_process_identity(worker_pid)
                .is_some_and(|identity| !matches!(identity.state, b'Z' | b'X'))
        {
            thread::sleep(Duration::from_millis(20));
        }
        assert!(worker_was_alive);
        assert!(
            read_process_identity(worker_pid)
                .is_none_or(|identity| matches!(identity.state, b'Z' | b'X'))
        );
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn malformed_or_truncated_proc_data_is_indeterminate() {
        assert_eq!(parse_stat("42 (worker) S 1"), None);
        assert_eq!(parse_stat("not proc stat"), None);
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn workspace_recovery_finds_non_http_descendants_after_runner_reaping_or_unjournaled_spawn() {
        for (persisted_runner, unassociated_member) in
            [(false, false), (true, false), (false, true), (true, true)]
        {
            let (store, state_path) = temporary_store();
            let root = state_path.parent().unwrap();
            std::fs::create_dir_all(root).unwrap();
            let marker_file = root.join("watcher.pid");
            let group_id = Uuid::new_v4();
            crate::workspace_session::journal_group(&store, group_id).unwrap();
            let mut group = store
                .load()
                .unwrap()
                .workspace_groups
                .remove(&group_id)
                .unwrap();
            let actual_path = root
                .join("custom-temp")
                .join(format!("nook-session-{group_id}/context.json"));
            assert_ne!(
                actual_path,
                crate::workspace_session::session_path(group_id)
            );
            group.session_path = Some(actual_path.clone().into_os_string());
            store
                .mutate(|registry| {
                    registry.workspace_groups.insert(group_id, group.clone());
                    Ok(())
                })
                .unwrap();
            group = store
                .load()
                .unwrap()
                .workspace_groups
                .remove(&group_id)
                .unwrap();
            let code = if unassociated_member {
                "import os,subprocess,time,uuid; env=os.environ.copy(); env['NOOK_WORKSPACE_PROCESS']=env.pop('NOOK_WORKSPACE_SESSION')+'/'+str(uuid.uuid4()); p=subprocess.Popen(['/bin/sleep','30'],start_new_session=True,env=env); open(os.environ['WATCHER_PID'],'w').write(str(p.pid)); time.sleep(.05)"
            } else {
                "import os,subprocess,time; p=subprocess.Popen(['/bin/sleep','30'],start_new_session=True); open(os.environ['WATCHER_PID'],'w').write(str(p.pid)); time.sleep(.05)"
            };
            let mut leader = super::spawn_managed_child_in(
                &["/usr/bin/python3".into(), "-c".into(), code.into()],
                &[
                    (
                        crate::workspace_session::SESSION_VARIABLE.into(),
                        actual_path.into_os_string(),
                    ),
                    ("WATCHER_PID".into(), marker_file.into_os_string()),
                ],
                group_id,
                None,
                None,
            )
            .unwrap();
            if persisted_runner {
                group.runner = Some(crate::state::Lease {
                    group_id: Some(group_id),
                    workspace_supervisor: Some(group.supervisor.clone()),
                    lan: false,
                    id: group_id,
                    hostname: String::new(),
                    target: String::new(),
                    scheme: Scheme::Http,
                    tls: false,
                    pid: leader.pid,
                    pgid: leader.pgid,
                    process_start_time_ticks: leader.start_time_ticks,
                    state: LeaseState::Starting,
                });
            }
            assert_eq!(leader.wait().unwrap(), 0);
            let watcher: u32 = std::fs::read_to_string(root.join("watcher.pid"))
                .unwrap()
                .parse()
                .unwrap();
            assert!(read_process_identity(leader.pid).is_none());
            assert!(
                read_process_identity(watcher).is_some_and(|identity| identity.pgid
                    == watcher as i32
                    && identity.state != b'Z')
            );
            super::cleanup_workspace_processes(group_id, &group).unwrap();
            assert!(
                read_process_identity(watcher)
                    .is_none_or(|identity| matches!(identity.state, b'Z' | b'X'))
            );
            std::fs::remove_dir_all(root).unwrap();
        }
    }

    #[test]
    #[cfg(target_os = "linux")]
    #[allow(unsafe_code)]
    fn workspace_namespace_read_handles_task_exit_after_lookup() {
        use std::fs;
        use std::os::fd::AsRawFd;
        use std::os::unix::fs::OpenOptionsExt;

        let namespace = fs::read_link("/proc/self/ns/user").unwrap();
        let mut child = spawn_child(&["/bin/sleep".into(), "30".into()], &[]).unwrap();
        let identity = read_process_identity(child.pid).unwrap();
        // Pin the resolved namespace dentry, as readlink's path lookup does
        // before proc_ns_readlink obtains the task. Then reap that task.
        let link = fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_PATH | libc::O_NOFOLLOW)
            .open(format!("/proc/{}/ns/user", child.pid))
            .unwrap();
        let read_link = || {
            let mut buffer = [0_u8; 128];
            let length = unsafe {
                libc::readlinkat(
                    link.as_raw_fd(),
                    c"".as_ptr(),
                    buffer.as_mut_ptr().cast(),
                    buffer.len(),
                )
            };
            if length < 0 {
                Err(std::io::Error::last_os_error())
            } else {
                Ok(std::path::PathBuf::from(OsString::from_vec(
                    buffer[..length as usize].to_vec(),
                )))
            }
        };
        assert!(
            super::workspace_namespace_matches(child.pid, identity, &namespace, read_link())
                .unwrap()
        );
        child.child.kill().unwrap();
        child.wait().unwrap();
        let result = read_link();
        assert_eq!(
            result.as_ref().unwrap_err().raw_os_error(),
            Some(libc::EACCES)
        );
        assert!(
            !super::workspace_namespace_matches(child.pid, identity, &namespace, result).unwrap()
        );
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn workspace_recovery_skips_protected_foreign_processes_but_keeps_owned_errors() {
        use std::fs;
        use std::io::{BufRead, BufReader, Write};
        use std::os::unix::fs::MetadataExt;
        use std::process::Stdio;

        // Namespace root may have CAP_SYS_PTRACE. Drop capabilities in an
        // isolated copy of this test so the /proc failures are real there too.
        const TEST: &str = "process::tests::workspace_recovery_skips_protected_foreign_processes_but_keeps_owned_errors";
        const ISOLATED: &str = "NOOK_PROTECTED_RECOVERY_TEST";
        if std::env::var_os(ISOLATED).is_none() {
            let status = Command::new("/usr/bin/python3")
                .args([
                    "-c",
                    "import ctypes,os,sys\nclass Header(ctypes.Structure): _fields_=[('version',ctypes.c_uint32),('pid',ctypes.c_int)]\nclass Data(ctypes.Structure): _fields_=[('effective',ctypes.c_uint32),('permitted',ctypes.c_uint32),('inheritable',ctypes.c_uint32)]\nlibc=ctypes.CDLL(None); assert libc.prctl(38,1,0,0,0)==0; assert libc.capset(ctypes.byref(Header(0x20080522,0)),ctypes.byref((Data*2)()))==0; os.execv(sys.argv[1],sys.argv[1:])",
                ])
                .arg(std::env::current_exe().unwrap())
                .args([TEST, "--exact", "--test-threads=1"])
                .env(ISOLATED, "1")
                .status()
                .unwrap();
            assert!(status.success());
            return;
        }

        let mut foreign = Command::new("/usr/bin/python3")
            .args([
                "-c",
                "import ctypes,time; assert ctypes.CDLL(None).prctl(4,0,0,0,0)==0; print('protected',flush=True); time.sleep(30)",
            ])
            .env_remove(crate::workspace_session::SESSION_VARIABLE)
            .env_remove(crate::workspace_session::PROCESS_VARIABLE)
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let mut ready = String::new();
        BufReader::new(foreign.stdout.take().unwrap())
            .read_line(&mut ready)
            .unwrap();
        assert_eq!(ready.trim(), "protected");
        let foreign_path = std::path::PathBuf::from(format!("/proc/{}", foreign.id()));
        assert_eq!(
            std::fs::metadata(&foreign_path).unwrap().uid(),
            std::fs::metadata("/proc/self").unwrap().uid(),
            "the protected process must reach the same-uid candidate checks"
        );
        assert_eq!(
            std::fs::read_link(foreign_path.join("ns/user"))
                .unwrap_err()
                .kind(),
            std::io::ErrorKind::PermissionDenied
        );
        assert_eq!(
            std::fs::read(foreign_path.join("environ"))
                .unwrap_err()
                .kind(),
            std::io::ErrorKind::PermissionDenied
        );

        let path = crate::workspace_session::session_path(Uuid::new_v4());
        let started_after = read_process_identity(std::process::id())
            .unwrap()
            .start_time_ticks;
        for claim in [None, Some(Uuid::new_v4())] {
            let token = claim.unwrap_or_else(Uuid::new_v4);
            let mut child = super::spawn_managed_child_in(
                &["/bin/sleep".into(), "30".into()],
                &[(
                    crate::workspace_session::PROCESS_VARIABLE.into(),
                    crate::workspace_session::member_process_marker(&path, token),
                )],
                token,
                None,
                None,
            )
            .unwrap();
            assert!(foreign.id() < child.pid);
            let result = super::cleanup_marked_workspace_processes(&path, claim, started_after);
            let killed = child.try_wait().unwrap().is_some();
            let foreign_alive = foreign.try_wait().unwrap().is_none();
            // Reap both children even when exercising the unfixed scanner.
            if !killed {
                child.child.kill().unwrap();
            }
            child.wait().unwrap();
            if result.is_err() || !killed || !foreign_alive {
                let _ = foreign.kill();
                foreign.wait().unwrap();
            }
            result.unwrap();
            assert!(killed, "the later marked child must be killed");
            assert!(foreign_alive, "the protected foreign process must survive");
        }
        foreign.kill().unwrap();
        foreign.wait().unwrap();

        // An exact marker observed before the process becomes protected still
        // must not turn a failed pidfd validation into a successful cleanup.
        let mut owned = Command::new("/usr/bin/python3")
            .args([
                "-c",
                "import ctypes,sys,time; print('ready',flush=True); sys.stdin.readline(); assert ctypes.CDLL(None).prctl(4,0,0,0,0)==0; print('protected',flush=True); time.sleep(30)",
            ])
            .env(crate::workspace_session::SESSION_VARIABLE, &path)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let mut output = BufReader::new(owned.stdout.take().unwrap());
        ready.clear();
        output.read_line(&mut ready).unwrap();
        assert_eq!(ready.trim(), "ready");
        let identity = read_process_identity(owned.id()).unwrap();
        let markers = super::RecoveryMarkers::new(&path, None);
        assert!(markers.matches(&std::fs::read(format!("/proc/{}/environ", owned.id())).unwrap()));
        owned.stdin.take().unwrap().write_all(b"protect\n").unwrap();
        ready.clear();
        output.read_line(&mut ready).unwrap();
        assert_eq!(ready.trim(), "protected");
        let namespace = fs::read_link("/proc/self/ns/user").unwrap();
        let protected_namespace = fs::read_link(format!("/proc/{}/ns/user", owned.id()));
        assert_eq!(
            protected_namespace.as_ref().unwrap_err().raw_os_error(),
            Some(libc::EACCES)
        );
        let namespace_result = super::workspace_namespace_matches(
            owned.id(),
            identity,
            &namespace,
            protected_namespace,
        );
        assert_eq!(
            namespace_result.unwrap_err().raw_os_error(),
            Some(libc::EACCES)
        );
        let result =
            super::signal_marked_process(owned.id(), identity, &markers, ProcessSignal::Kill);
        let owned_alive = owned.try_wait().unwrap().is_none();
        owned.kill().unwrap();
        owned.wait().unwrap();
        assert_eq!(
            result.unwrap_err().kind(),
            std::io::ErrorKind::PermissionDenied
        );
        assert!(owned_alive, "failed ownership validation must not signal");
    }

    #[test]
    #[cfg(target_os = "linux")]
    #[allow(unsafe_code)]
    fn workspace_environment_recheck_handles_exit_after_identity_validation() {
        use std::io::{BufRead, BufReader, Write};

        // Clear capabilities even under namespace root, so protected /proc
        // reads really return EACCES rather than depending on the test user.
        const TEST: &str =
            "process::tests::workspace_environment_recheck_handles_exit_after_identity_validation";
        const ISOLATED: &str = "NOOK_ENVIRONMENT_RECHECK_TEST";
        if std::env::var_os(ISOLATED).is_none() {
            let status = Command::new("/usr/bin/python3")
                .args([
                    "-c",
                    "import ctypes,os,sys; libc=ctypes.CDLL(None); assert libc.prctl(38,1,0,0,0)==0; header=(ctypes.c_uint32*2)(0x20080522,0); data=(ctypes.c_uint32*6)(); assert libc.capset(ctypes.byref(header),ctypes.byref(data))==0; os.execv(sys.argv[1],sys.argv[1:])",
                ])
                .arg(std::env::current_exe().unwrap())
                .args([TEST, "--exact", "--test-threads=1"])
                .env(ISOLATED, "1")
                .status()
                .unwrap();
            assert!(status.success());
            return;
        }

        let path = crate::workspace_session::session_path(Uuid::new_v4());
        let markers = super::RecoveryMarkers::new(&path, None);
        let mut child = Command::new("/usr/bin/python3")
            .args([
                "-c",
                "import ctypes,sys,time; print('ready',flush=True); sys.stdin.readline(); assert ctypes.CDLL(None).prctl(4,0,0,0,0)==0; print('protected',flush=True); time.sleep(30)",
            ])
            .env(crate::workspace_session::SESSION_VARIABLE, &path)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let mut output = BufReader::new(child.stdout.take().unwrap());
        let mut ready = String::new();
        output.read_line(&mut ready).unwrap();
        assert_eq!(ready.trim(), "ready");
        let pid = child.id();
        let identity = read_process_identity(pid).unwrap();
        let environment = || std::fs::read(format!("/proc/{pid}/environ"));
        assert!(
            super::workspace_environment_matches(pid, identity, &markers, environment()).unwrap()
        );
        child.stdin.take().unwrap().write_all(b"protect\n").unwrap();
        ready.clear();
        output.read_line(&mut ready).unwrap();
        assert_eq!(ready.trim(), "protected");
        let live_error =
            super::workspace_environment_matches(pid, identity, &markers, environment())
                .unwrap_err();
        let denied_environment = environment();
        assert_eq!(
            denied_environment.as_ref().unwrap_err().raw_os_error(),
            Some(libc::EACCES)
        );
        assert!(child.try_wait().unwrap().is_none());
        child.kill().unwrap();
        // Defer classifying a real EACCES until the task has exited. Keep its
        // zombie entry to test the state check rather than a missing path.
        let mut info = std::mem::MaybeUninit::<libc::siginfo_t>::uninit();
        assert_eq!(
            unsafe {
                libc::waitid(
                    libc::P_PID,
                    pid,
                    info.as_mut_ptr(),
                    libc::WEXITED | libc::WNOWAIT,
                )
            },
            0
        );
        assert_eq!(read_process_identity(pid).unwrap().state, b'Z');
        let recovered =
            super::workspace_environment_matches(pid, identity, &markers, denied_environment);
        // A fresh zombie environ open returns EACCES for an ordinary uid,
        // but can reach ESRCH instead when namespace root passes DAC checks.
        let after_exit =
            super::workspace_environment_matches(pid, identity, &markers, environment());
        child.wait().unwrap();
        assert_eq!(live_error.kind(), std::io::ErrorKind::PermissionDenied);
        assert_eq!(live_error.raw_os_error(), Some(libc::EACCES));
        assert!(!recovered.unwrap());
        assert!(!after_exit.unwrap());
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn workspace_marker_kill_rejects_reused_identity_and_foreign_marker() {
        let group_id = Uuid::new_v4();
        let marker =
            super::RecoveryMarkers::new(&crate::workspace_session::session_path(group_id), None);
        let mut child = super::spawn_managed_child_in(
            &["/bin/sleep".into(), "30".into()],
            &[(
                crate::workspace_session::SESSION_VARIABLE.into(),
                crate::workspace_session::session_path(group_id).into_os_string(),
            )],
            group_id,
            None,
            None,
        )
        .unwrap();
        let identity = read_process_identity(child.pid).unwrap();
        let reused = ProcIdentity {
            start_time_ticks: identity.start_time_ticks + 1,
            ..identity
        };
        assert!(
            !super::signal_marked_process(child.pid, reused, &marker, ProcessSignal::Kill).unwrap()
        );
        assert!(
            !super::signal_marked_process(
                child.pid,
                identity,
                &super::RecoveryMarkers::new(std::path::Path::new("/foreign/context.json"), None),
                ProcessSignal::Kill
            )
            .unwrap()
        );
        assert!(child.try_wait().unwrap().is_none());
        // Scheduling state is transient; an otherwise valid sleeping/running
        // identity must still authorize the pinned process.
        let changed_state = ProcIdentity {
            state: b'R',
            ..read_process_identity(child.pid).unwrap()
        };
        assert!(
            super::signal_marked_process(child.pid, changed_state, &marker, ProcessSignal::Kill)
                .unwrap()
        );
        assert_eq!(child.wait().unwrap(), 128 + libc::SIGKILL);
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
        assert_eq!(first.pgid, second.pgid);
        assert_eq!(first.start_time_ticks, second.start_time_ticks);
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
    fn framework_alignment_appends_flags_and_environment() {
        let (store, path) = temporary_store();
        let marker = path.parent().unwrap().join("argv.env");
        let code = format!(
            "import os,sys; open(r'{}','w').write('\\n'.join(sys.argv)+'\\n'+os.environ.get('__VITE_ADDITIONAL_SERVER_ALLOWED_HOSTS',''))",
            marker.display()
        );
        let mut config = run_config(vec!["/usr/bin/python3", "-c", &code], 1);
        config.framework =
            crate::framework::FrameworkChoice::Forced(crate::framework::Framework::Vite);
        let mut routes = Routes::default();
        let mut running = start_run(&config, &store, &mut routes).unwrap();
        let _ = running.wait_for_readiness(&store, |_| {});
        running.finish(&store, &mut routes).unwrap();
        let dumped = std::fs::read_to_string(&marker).unwrap();
        assert!(
            dumped.contains("api.localhost"),
            "forced Vite still injects allowed-hosts env: {dumped}"
        );
        assert!(
            !dumped.contains("--host"),
            "argv flags are not added unless the CLI is vite/nuxt/astro: {dumped}"
        );
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn undetected_command_keeps_argv_and_base_environment() {
        let (store, path) = temporary_store();
        let marker = path.parent().unwrap().join("plain.env");
        let code = format!(
            "import os; keys=['PORT','HOST','NOOK_URL','NUXT_HOST','NUXT_PORT','NITRO_HOST','NITRO_PORT','HOSTNAME','__VITE_ADDITIONAL_SERVER_ALLOWED_HOSTS']; argv=[part.decode() for part in open('/proc/self/cmdline','rb').read().split(b'\\0') if part]; open(r'{}','w').write('\\n'.join(argv)+'\\n---\\n'+'\\n'.join(k+'='+os.environ.get(k,'') for k in keys))",
            marker.display()
        );
        let mut config = run_config(vec!["/usr/bin/python3", "-c", &code, "--bind", "{port}"], 1);
        config.framework = crate::framework::FrameworkChoice::Auto;
        let mut routes = Routes::default();
        let mut running = start_run(&config, &store, &mut routes).unwrap();
        let port = running.port.to_string();
        let _ = running.wait_for_readiness(&store, |_| {});
        running.finish(&store, &mut routes).unwrap();
        let dumped = std::fs::read_to_string(&marker).unwrap();
        let (argv_dump, env_dump) = dumped.split_once("\n---\n").unwrap();
        assert_eq!(
            argv_dump.lines().collect::<Vec<_>>(),
            vec![
                "/usr/bin/python3",
                "-c",
                code.as_str(),
                "--bind",
                port.as_str()
            ]
        );
        let values: std::collections::BTreeMap<_, _> = env_dump
            .lines()
            .map(|line| line.split_once('=').unwrap())
            .collect();
        assert_eq!(values["PORT"], port);
        assert_eq!(values["HOST"], "127.0.0.1");
        assert_eq!(values["NOOK_URL"], "https://api.localhost");
        for key in [
            "NUXT_HOST",
            "NUXT_PORT",
            "NITRO_HOST",
            "NITRO_PORT",
            "HOSTNAME",
            "__VITE_ADDITIONAL_SERVER_ALLOWED_HOSTS",
        ] {
            assert_eq!(values[key], std::env::var(key).unwrap_or_default());
        }
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
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

    const RAW_STDIN_SESSION_ENV: &str = "NOOK_TEST_RAW_STDIN_SESSION";

    #[test]
    fn raw_tty_child_is_not_stopped_by_process_group() {
        if std::env::var_os(RAW_STDIN_SESSION_ENV).is_some() {
            raw_tty_session_leader();
            return;
        }
        if Command::new("python3")
            .args(["-c", "import pty"])
            .status()
            .map(|status| !status.success())
            .unwrap_or(true)
        {
            return;
        }
        let exe = std::env::current_exe().unwrap();
        let launcher = r#"
import os, pty, sys
exe = sys.argv[1]
env = os.environ.copy()
env["NOOK_TEST_RAW_STDIN_SESSION"] = "1"
pid, _fd = pty.fork()
if pid == 0:
    os.execve(exe, [exe, "--exact", "process::tests::raw_tty_child_is_not_stopped_by_process_group", "--test-threads=1", "--nocapture"], env)
    os._exit(127)
_, status = os.waitpid(pid, 0)
if os.WIFEXITED(status):
    sys.exit(os.WEXITSTATUS(status))
sys.exit(1)
"#;
        let output = Command::new("python3")
            .args(["-c", launcher])
            .arg(exe)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "raw-tty child was stopped or failed: status={:?} stdout={} stderr={}",
            output.status.code(),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn raw_tty_session_leader() {
        let identity_self = read_process_identity(std::process::id()).unwrap();
        assert_ne!(
            identity_self.state, b'T',
            "session leader must own a running controlling terminal"
        );
        let mut child = spawn_child(
            &[
                OsString::from("/usr/bin/python3"),
                OsString::from("-c"),
                OsString::from(
                    "import sys, tty, time\ntty.setraw(0)\nsys.stderr.write('setraw-ok\\n')\nsys.stderr.flush()\ntime.sleep(2)\n",
                ),
            ],
            &[],
        )
        .unwrap();
        let started = Instant::now();
        loop {
            let identity = read_process_identity(child.pid)
                .expect("raw-tty child should remain visible in /proc");
            assert_ne!(
                identity.state, b'T',
                "child in a new process group was stopped (SIGTTIN/SIGTTOU) while using raw stdin"
            );
            if child.has_exited().unwrap() || started.elapsed() >= Duration::from_millis(400) {
                break;
            }
            thread::sleep(Duration::from_millis(20));
        }
        if !child.has_exited().unwrap() {
            child.signal(ProcessSignal::Terminate).unwrap();
        }
        let deadline = Instant::now() + Duration::from_secs(3);
        while child.try_wait().unwrap().is_none() {
            assert!(Instant::now() < deadline, "polled child did not exit");
            thread::sleep(Duration::from_millis(20));
        }
        assert!(
            child.terminal.is_none(),
            "polling must restore the terminal"
        );

        // Native workspace apps have null stdin, even when Nook runs in a TTY.
        // Neither spawn nor the polling loop may hand them its foreground.
        let mut native = super::spawn_managed_child_in(
            &["/bin/sleep".into(), "10".into()],
            &[],
            Uuid::new_v4(),
            None,
            Some("native"),
        )
        .unwrap();
        assert!(native.try_wait().unwrap().is_none());
        let foreground = Command::new("python3")
            .args(["-c", "import os; assert os.tcgetpgrp(0) == os.getpgrp()"])
            .stdin(std::process::Stdio::inherit())
            .output()
            .unwrap();
        native.signal(ProcessSignal::Kill).unwrap();
        native.wait().unwrap();
        native.join_output();
        assert!(foreground.status.success(), "native app took the terminal");
    }

    #[test]
    fn terminal_is_claimed_only_when_this_process_group_owns_it() {
        assert!(owns_foreground_terminal(42, 42));
        assert!(!owns_foreground_terminal(42, 7));
        assert!(!owns_foreground_terminal(-1, 7));
    }

    #[test]
    fn parent_handoff_does_not_steal_a_descendant_foreground() {
        assert!(parent_may_reassign_foreground(7, 7, 11));
        assert!(parent_may_reassign_foreground(11, 7, 11));
        assert!(!parent_may_reassign_foreground(13, 7, 11));
    }

    #[test]
    fn stopped_states_are_job_control_stops() {
        assert!(process_is_stopped_state(b'T'));
        assert!(!process_is_stopped_state(b't'));
        assert!(!process_is_stopped_state(b'S'));
        assert!(!process_is_stopped_state(b'R'));
    }

    const JOB_CONTROL_SESSION_ENV: &str = "NOOK_TEST_JOB_CONTROL_SESSION";

    #[test]
    fn stopped_child_suspends_until_continued() {
        if std::env::var_os(JOB_CONTROL_SESSION_ENV).is_some() {
            job_control_session_leader();
            return;
        }
        if Command::new("python3")
            .args(["-c", "import pty"])
            .status()
            .map(|status| !status.success())
            .unwrap_or(true)
        {
            return;
        }
        let exe = std::env::current_exe().unwrap();
        let launcher = r#"
import fcntl, os, select, signal, sys, termios, time
exe = sys.argv[1]
env = os.environ.copy()
env["NOOK_TEST_JOB_CONTROL_SESSION"] = "1"
master, slave = os.openpty()
shell = os.fork()
if shell == 0:
    os.close(master)
    os.setsid()
    fcntl.ioctl(slave, termios.TIOCSCTTY, 0)
    os.dup2(slave, 0)
    os.dup2(slave, 1)
    os.dup2(slave, 2)
    if slave > 2:
        os.close(slave)
    nook = os.fork()
    if nook == 0:
        os.setpgid(0, 0)
        os.execve(exe, [exe, "--exact", "process::tests::stopped_child_suspends_until_continued", "--test-threads=1", "--nocapture"], env)
        os._exit(127)
    os.setpgid(nook, nook)
    signal.signal(signal.SIGTTOU, signal.SIG_IGN)
    os.tcsetpgrp(0, nook)
    saw_stop = False
    deadline = time.time() + 8
    while time.time() < deadline:
        wpid, status = os.waitpid(nook, os.WUNTRACED | os.WNOHANG)
        if wpid == 0:
            time.sleep(0.05)
            continue
        if os.WIFSTOPPED(status):
            saw_stop = True
            os.kill(nook, signal.SIGCONT)
            continue
        if os.WIFEXITED(status):
            if not saw_stop:
                os.write(2, b"wrapper never stopped\n")
                os._exit(2)
            os._exit(os.WEXITSTATUS(status))
        os._exit(1)
    os.write(2, b"timed out waiting for job-control stop\n")
    os._exit(3)
os.close(slave)
deadline = time.time() + 9
while time.time() < deadline:
    readable, _, _ = select.select([master], [], [], 0.05)
    if master in readable:
        try:
            chunk = os.read(master, 4096)
        except OSError:
            chunk = b""
        if chunk:
            sys.stderr.buffer.write(chunk)
            sys.stderr.buffer.flush()
    wpid, status = os.waitpid(shell, os.WNOHANG)
    if wpid == 0:
        continue
    if os.WIFEXITED(status):
        sys.exit(os.WEXITSTATUS(status))
    sys.exit(1)
sys.stderr.write("timed out waiting for session shell\n")
sys.exit(3)
"#;
        let output = Command::new("python3")
            .args(["-c", launcher])
            .arg(exe)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "job-control child did not suspend the wrapper: status={:?} stdout={} stderr={}",
            output.status.code(),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn job_control_session_leader() {
        let mut sibling = Command::new("/bin/sleep").arg("30").spawn().unwrap();
        let sibling_pid = sibling.id();
        let sibling_ready = Instant::now() + Duration::from_secs(1);
        while read_process_identity(sibling_pid).is_none() {
            assert!(
                Instant::now() < sibling_ready,
                "pipeline sibling did not start"
            );
            thread::sleep(Duration::from_millis(20));
        }
        let mut child =
            spawn_child(&[OsString::from("/bin/sleep"), OsString::from("30")], &[]).unwrap();
        send_group_signal(child.pgid, libc::SIGTSTP).unwrap();
        let stop_deadline = Instant::now() + Duration::from_secs(1);
        while !process_is_stopped(child.pid) {
            assert!(
                Instant::now() < stop_deadline,
                "child did not stop after SIGTSTP"
            );
            thread::sleep(Duration::from_millis(20));
        }
        assert!(child.try_wait().unwrap().is_none());
        // The shell can resume the wrapper before its sibling has processed
        // the group stop signal. Wait for the observable state, not scheduling.
        let sibling_stop_deadline = Instant::now() + Duration::from_secs(1);
        while !process_is_stopped(sibling_pid) {
            assert!(
                Instant::now() < sibling_stop_deadline,
                "wrapper SIGTSTP must stop the rest of its process group"
            );
            thread::sleep(Duration::from_millis(20));
        }
        let continue_deadline = Instant::now() + Duration::from_secs(1);
        while process_is_stopped(child.pid) {
            assert!(
                Instant::now() < continue_deadline,
                "child did not continue after wrapper SIGCONT"
            );
            thread::sleep(Duration::from_millis(20));
        }
        let _ = sibling.kill();
        let _ = sibling.wait();
        child.signal(ProcessSignal::Terminate).unwrap();
        let _ = child.wait();
    }

    #[test]
    fn group_signal_returns_conventional_signal_exit_code() {
        let mut child =
            spawn_child(&[OsString::from("/bin/sleep"), OsString::from("10")], &[]).unwrap();
        child.signal(ProcessSignal::Terminate).unwrap();
        assert_eq!(child.wait().unwrap(), 128 + libc::SIGTERM);
    }

    #[test]
    fn owned_cleanup_refuses_the_callers_or_supervisors_group() {
        let identity = read_process_identity(std::process::id()).unwrap();
        let mut lease = crate::state::Lease {
            group_id: None,
            workspace_supervisor: None,
            lan: false,
            id: Uuid::new_v4(),
            hostname: "app.localhost".into(),
            target: "http://127.0.0.1:3000".into(),
            scheme: Scheme::Http,
            tls: true,
            pid: std::process::id(),
            pgid: identity.pgid,
            process_start_time_ticks: identity.start_time_ticks,
            state: LeaseState::Ready,
        };
        assert!(super::kill_owned_lease(&lease).is_err());
        let mut child = spawn_child(&["/bin/sleep".into(), "10".into()], &[]).unwrap();
        lease.pid = child.pid;
        lease.pgid = child.pgid;
        lease.process_start_time_ticks = child.start_time_ticks;
        lease.workspace_supervisor = Some(crate::state::WorkspaceSupervisor {
            pid: child.pid,
            process_start_time_ticks: child.start_time_ticks,
        });
        let refused = super::kill_owned_lease(&lease).is_err();
        let still_running = child.try_wait().unwrap().is_none();
        lease.workspace_supervisor = None;
        lease.process_start_time_ticks += 1;
        let reused = super::signal_owned_lease(&lease, ProcessSignal::Terminate).unwrap();
        let survived_reused = child.try_wait().unwrap().is_none();
        lease.process_start_time_ticks -= 1;
        let delivered = super::signal_owned_lease(&lease, ProcessSignal::Terminate).unwrap();
        let code = child.wait().unwrap();
        let missing = super::signal_owned_lease(&lease, ProcessSignal::Terminate).unwrap();
        assert!(refused && still_running);
        assert!(
            !reused && survived_reused,
            "a reused identity must report no delivery"
        );
        assert!(delivered, "a verified group signal must report delivery");
        assert_eq!(code, 128 + libc::SIGTERM);
        assert!(!missing, "a reaped process must report no delivery");
    }

    #[test]
    fn owned_group_signal_handles_exit_after_verification() {
        let mut child = spawn_child(&["/bin/sleep".into(), "30".into()], &[]).unwrap();
        let lease = crate::state::Lease {
            group_id: None,
            workspace_supervisor: None,
            lan: false,
            id: Uuid::new_v4(),
            hostname: "app.localhost".into(),
            target: "http://127.0.0.1:3000".into(),
            scheme: Scheme::Http,
            tls: false,
            pid: child.pid,
            pgid: child.pgid,
            process_start_time_ticks: child.start_time_ticks,
            state: LeaseState::Ready,
        };
        assert_eq!(super::lease_liveness(&lease), Liveness::Alive);
        child.child.kill().unwrap();
        child.wait().unwrap();

        // Model exit/reaping after ownership verification, before dispatch.
        // Calling signal_owned_lease now would only exercise its early Dead path.
        let result = super::signal_lease(&lease, ProcessSignal::Terminate);
        assert_eq!(
            result.as_ref().unwrap_err().raw_os_error(),
            Some(libc::ESRCH)
        );
        assert!(!super::owned_group_signal_result(result).unwrap());
        assert!(matches!(
            super::owned_group_signal_result(Err(std::io::Error::from_raw_os_error(libc::EPERM))),
            Err(super::Error::Spawn(error)) if error.raw_os_error() == Some(libc::EPERM)
        ));
    }

    #[test]
    fn prepared_port_can_be_released_twice_before_member_joins() {
        let (store, path) = temporary_store();
        let config = run_config(vec!["/bin/sleep", "10"], 30);
        let operations = store.lock_operations().unwrap();
        let reservation = reserve_port(None, false).unwrap();
        let port = reservation.port;
        let mut prepared = super::prepare_run(
            &config,
            reservation,
            &store,
            &mut Routes::default(),
            &operations,
            &mut LocalOnly,
        )
        .unwrap();
        assert!(TcpListener::bind((Ipv4Addr::LOCALHOST, port)).is_err());
        prepared.release_port();
        prepared.release_port();
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, port)).unwrap();
        drop(listener);
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn owned_cleanup_kills_the_members_entire_verified_group() {
        for zombie_leader in [false, true] {
            let (store, path) = temporary_store();
            let marker = path.with_extension("descendant");
            let config = run_config(
                vec![
                    "/bin/sh",
                    "-c",
                    "sleep 10 & echo $! > \"$1\"; wait",
                    "nook-test",
                    marker.to_str().unwrap(),
                ],
                30,
            );
            let mut running = start_run(&config, &store, &mut Routes::default()).unwrap();
            let deadline = Instant::now() + Duration::from_secs(2);
            let descendant = loop {
                if let Ok(contents) = std::fs::read_to_string(&marker)
                    && let Ok(pid) = contents.trim().parse::<u32>()
                {
                    break pid;
                }
                assert!(
                    Instant::now() < deadline,
                    "member did not launch its descendant"
                );
                thread::sleep(Duration::from_millis(5));
            };
            let identity = read_process_identity(descendant).unwrap();
            assert_eq!(identity.pgid, running.child.pgid);
            let lease = store
                .load()
                .unwrap()
                .leases
                .remove(&running.lease_id)
                .unwrap();
            if zombie_leader {
                // Kill only the leader and leave it unreaped: its recorded raw
                // identity still proves ownership of the surviving descendant.
                running.child.child.kill().unwrap();
                while super::lease_liveness(&lease) != Liveness::Dead {
                    assert!(Instant::now() < deadline);
                    thread::sleep(Duration::from_millis(5));
                }
                assert_eq!(read_process_identity(lease.pid).unwrap().state, b'Z');
                assert_eq!(
                    read_process_identity(descendant).unwrap().start_time_ticks,
                    identity.start_time_ticks
                );
            }
            super::kill_owned_lease(&lease).unwrap();
            assert_eq!(running.child.wait().unwrap(), 128 + libc::SIGKILL);
            while read_process_identity(descendant).is_some_and(|current| {
                identity_liveness(current, identity.pgid, identity.start_time_ticks)
                    == Liveness::Alive
            }) {
                assert!(
                    Instant::now() < deadline,
                    "member descendant survived group cleanup"
                );
                thread::sleep(Duration::from_millis(5));
            }
            std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
        }
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
    fn forced_replacement_rejects_a_different_project_with_the_same_lan_name() {
        let (store, path) = temporary_store();
        let mut routes = Routes::default();
        let mut config = run_config(vec!["/bin/sleep", "10"], 30);
        config.lan = true;
        let mut first = start_run(&config, &store, &mut routes).unwrap();
        let before = store.load().unwrap();
        config.hostname = "api.local.localhost".into();
        config.force = true;
        let result = start_run(&config, &store, &mut routes);
        let rejected = result.is_err();
        let unchanged = store.load().unwrap() == before;
        if let Ok(mut unexpected) = result {
            unexpected.child.signal(ProcessSignal::Terminate).unwrap();
            unexpected.finish(&store, &mut routes).unwrap();
        }
        first.child.signal(ProcessSignal::Terminate).unwrap();
        first.finish(&store, &mut routes).unwrap();
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
        assert!(
            rejected,
            "--force must not steal another project's derived .local name"
        );
        assert!(
            unchanged,
            "a rejected collision must leave the registry unchanged"
        );
    }

    #[test]
    fn displaced_lan_supervisor_withdraws_mdns_before_its_child_exits() {
        for waiting_for_readiness in [false, true] {
            let (store, path) = temporary_store();
            let mut routes = Routes::default();
            let mut config = run_config(vec!["/bin/sleep", "2"], 30);
            config.lan = true;
            let mut first = start_run(&config, &store, &mut routes).unwrap();
            let daemon = first
                .lan
                .as_ref()
                .expect("mDNS must start")
                .daemon_for_test();
            let other_store = Store::new(path.clone());
            let replace = thread::spawn(move || {
                let mut config = run_config(vec!["/bin/true"], 30);
                config.force = true;
                let mut routes = Routes::default();
                let mut replacement = start_run(&config, &other_store, &mut routes).unwrap();
                replacement.finish(&other_store, &mut routes).unwrap();
                let deadline = Instant::now() + Duration::from_millis(1200);
                while Instant::now() < deadline {
                    if daemon
                        .status()
                        .unwrap()
                        .recv_timeout(Duration::from_secs(1))
                        .unwrap()
                        == mdns_sd::DaemonStatus::Shutdown
                    {
                        return true;
                    }
                    thread::sleep(Duration::from_millis(20));
                }
                false
            });
            if waiting_for_readiness {
                assert!(!first.wait_for_readiness(&store, |_| {}).unwrap());
            }
            assert_eq!(first.finish(&store, &mut routes).unwrap().exit_code, 0);
            let withdrawn = replace.join().unwrap();
            std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
            assert!(
                withdrawn,
                "displaced mDNS remains active (readiness={waiting_for_readiness})"
            );
        }
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
        let mut running =
            start_run_with_hook(&config, &store, &mut routes, &mut LocalOnly, |port| {
                *stolen.borrow_mut() =
                    Some(TcpListener::bind((Ipv4Addr::LOCALHOST, port)).unwrap());
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
    fn workspace_stop_requires_an_associated_member_and_consistent_group() {
        use crate::state::{Lease, WorkspaceGroup, WorkspaceSupervisor};

        let (store, path) = temporary_store();
        let group_id = Uuid::new_v4();
        let supervisor = WorkspaceSupervisor {
            pid: 100,
            process_start_time_ticks: 10,
        };
        let runner = Lease {
            id: group_id,
            group_id: Some(group_id),
            workspace_supervisor: Some(supervisor.clone()),
            lan: false,
            hostname: String::new(),
            target: String::new(),
            scheme: Scheme::Http,
            tls: false,
            pid: 200,
            pgid: 200,
            process_start_time_ticks: 20,
            state: LeaseState::Starting,
        };
        let mut member = runner.clone();
        member.id = Uuid::new_v4();
        member.hostname = "api.localhost".into();
        member.target = "http://127.0.0.1:3000".into();
        let group = WorkspaceGroup {
            supervisor,
            runner: Some(runner),
            session_path: None,
        };
        for case in [
            "pending",
            "pending_ready",
            "runner_pid",
            "runner_group",
            "missing_group",
            "missing_supervisor",
            "wrong_supervisor",
            "wrong_runner_owner",
            "wrong_runner_group",
            "wrong_runner_supervisor",
            "active_starting",
            "active_ready",
            "native_starting",
        ] {
            let mut lease = member.clone();
            let mut journal = group.clone();
            if !matches!(case, "pending" | "pending_ready") {
                lease.pid = 300;
                lease.pgid = 300;
                lease.process_start_time_ticks = 30;
            }
            match case {
                "pending_ready" | "active_ready" => lease.state = LeaseState::Ready,
                // Partial identity matches must fail closed, even with a different start time.
                "runner_pid" => lease.pid = 200,
                "runner_group" => lease.pgid = 200,
                "missing_supervisor" => lease.workspace_supervisor = None,
                "wrong_supervisor" => {
                    lease
                        .workspace_supervisor
                        .as_mut()
                        .unwrap()
                        .process_start_time_ticks += 1;
                }
                "wrong_runner_owner" => journal.runner.as_mut().unwrap().id = Uuid::new_v4(),
                "wrong_runner_group" => journal.runner.as_mut().unwrap().group_id = None,
                "wrong_runner_supervisor" => {
                    journal.runner.as_mut().unwrap().workspace_supervisor = None;
                }
                "native_starting" => journal.runner = None,
                _ => {}
            }
            store
                .mutate(|registry| {
                    registry.leases.insert(lease.id, lease.clone());
                    registry.workspace_groups.clear();
                    if case != "missing_group" {
                        registry.workspace_groups.insert(group_id, journal.clone());
                    }
                    Ok(())
                })
                .unwrap();
            let before = store.load().unwrap();
            for force in [false, true] {
                let mut system = ControlledStop::default();
                let result = stop_managed(&store, "api.localhost", force, &mut system);
                if matches!(case, "active_starting" | "active_ready" | "native_starting") {
                    assert!(result.is_ok(), "{case}: {result:?}");
                    assert_eq!(system.signals[0], ProcessSignal::Terminate, "{case}");
                    assert_eq!(system.signals.len(), if force { 2 } else { 1 }, "{case}");
                } else {
                    assert!(
                        result.is_err(),
                        "{case} force={force} must not signal the runner"
                    );
                    assert!(system.signals.is_empty(), "{case}");
                    assert_eq!(system.alive_checks, 0, "{case}");
                }
                assert_eq!(store.load().unwrap(), before, "{case}");
            }
        }
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
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
            tailscale: false,
            funnel: false,
            lan: false,
            app_port: None,
            strict_port: false,
            force: false,
            readiness_warn_after_seconds: readiness,
            bind_address: IpAddr::V4(Ipv4Addr::LOCALHOST),
            ignored_local_config: None,
            framework: crate::framework::FrameworkChoice::Disabled,
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
        Liveness, ProcessSignal, lease_liveness, read_process_identity,
        signal_with_forced_fallback, spawn_child, substitute_port, windows_batch_command_line,
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
    fn owned_cleanup_requires_member_identity_and_matching_named_job() {
        let id = Uuid::new_v4();
        let foreign_id = Uuid::new_v4();
        let argv = [
            "cmd.exe".into(),
            "/D".into(),
            "/C".into(),
            "ping -n 20 127.0.0.1 >nul".into(),
        ];
        let mut child = super::spawn_managed_child(&argv, &[], id).unwrap();
        let mut foreign = super::spawn_managed_child(&argv, &[], foreign_id).unwrap();
        let mut lease = Lease {
            group_id: None,
            workspace_supervisor: None,
            lan: false,
            id: foreign_id,
            hostname: "app.localhost".into(),
            target: "http://127.0.0.1:3000".into(),
            scheme: Scheme::Http,
            tls: true,
            pid: child.pid,
            pgid: child.pgid,
            process_start_time_ticks: child.start_time_ticks,
            state: LeaseState::Ready,
        };
        assert!(
            super::kill_owned_lease(&lease).is_err(),
            "a different named job must not be killed"
        );
        assert!(child.try_wait().unwrap().is_none());
        let foreign_survived = foreign.try_wait().unwrap().is_none();
        foreign.signal(ProcessSignal::Kill).unwrap();
        foreign.wait().unwrap();
        assert!(
            foreign_survived,
            "a mismatched job must not receive a cleanup signal"
        );
        lease.id = id;
        lease.process_start_time_ticks += 1;
        super::kill_owned_lease(&lease).unwrap();
        assert!(
            child.try_wait().unwrap().is_none(),
            "a reused PID must not be signaled"
        );
        lease.process_start_time_ticks -= 1;
        super::kill_owned_lease(&lease).unwrap();
        assert_eq!(child.wait().unwrap(), 1);
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
    fn member_directory_wins_over_root_executables_and_relative_path_entries() {
        let root = std::env::temp_dir().join(format!("nook member shim {}", Uuid::new_v4()));
        let member = root.join("apps/api");
        std::fs::create_dir_all(member.join("bin")).unwrap();
        std::fs::write(root.join("dev.cmd"), "@exit /b 9\r\n").unwrap();
        std::fs::write(member.join("dev.cmd"), "@exit /b 7\r\n").unwrap();
        std::fs::write(member.join("bin/tool.cmd"), "@exit /b 8\r\n").unwrap();
        let environment = [
            (OsString::from("PATH"), root.as_os_str().to_owned()),
            (OsString::from("PATHEXT"), OsString::from(".CMD")),
        ];
        for program in ["dev.cmd", "./dev.cmd"] {
            let mut child = super::spawn_managed_child_in(
                &[program.into()],
                &environment,
                Uuid::new_v4(),
                Some(&member),
                None,
            )
            .unwrap();
            assert_eq!(child.wait().unwrap(), 7);
        }
        let resolved = super::resolve_windows_program(
            std::ffi::OsStr::new("tool"),
            &[(OsString::from("PATH"), OsString::from("bin"))],
            Some(&member),
        )
        .unwrap();
        assert_eq!(resolved, member.join("bin/tool.CMD"));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn batch_shim_arguments_reject_line_breaks() {
        for argument in ["before\nafter", "before\rafter"] {
            let error = windows_batch_command_line(
                std::ffi::OsStr::new("shim.cmd"),
                &[OsString::from(argument)],
                "NOOK_INTERNAL_PERCENT_TEST",
            )
            .expect_err("line breaks must not enter a cmd.exe command string");
            assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
            assert!(error.to_string().contains("line breaks"));
        }
    }

    #[test]
    fn failed_foreground_break_forces_job_termination() {
        let mut delivered = Vec::new();
        signal_with_forced_fallback(ProcessSignal::Interrupt, |signal| {
            delivered.push(signal);
            if signal == ProcessSignal::Interrupt {
                Err(std::io::Error::other("console is not attached"))
            } else {
                Ok(())
            }
        })
        .unwrap();
        assert_eq!(delivered, [ProcessSignal::Interrupt, ProcessSignal::Kill]);
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
            group_id: None,
            workspace_supervisor: None,
            lan: false,
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
