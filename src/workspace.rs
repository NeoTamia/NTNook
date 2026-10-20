//! Native workspace planning and foreground supervision.

use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::io;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::thread;
use std::time::{Duration, Instant};

use uuid::Uuid;

use crate::process::{self, ForwardedSignals, ProcessSignal, RunExposure, RunOutcome};
use crate::reconcile::RouteBackend;
use crate::state::{
    Lease, LeaseState, PendingOperation, PendingOperationKind, Store, WorkspaceSupervisor,
};
use crate::workspace_config::{ResolvedWorkspaceConfig, WorkspaceApp};
use crate::workspace_session::{SESSION_VARIABLE, SessionApp, SessionMember, SessionOwner};

/// All actual ports and interpolated argv/env are available before installing routes.
/// Dropping this plan releases every reservation without starting an application.
pub(crate) struct PreparedWorkspace {
    pub(crate) root: PathBuf,
    pub(crate) group_id: Uuid,
    pub(crate) apps: Vec<PreparedApp>,
    signals: ForwardedSignals,
}

pub(crate) struct PreparedApp {
    pub(crate) app: WorkspaceApp,
    pub(crate) port: u16,
    pub(crate) environment: Vec<(OsString, OsString)>,
    reservation: Option<process::PortReservation>,
}

fn invalid(message: impl Into<String>) -> crate::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message.into()).into()
}

/// Even a failed association or output operation must reap the separate server
/// group before its session claim can be released.
struct ChildTree(process::ManagedChild, bool);

impl ChildTree {
    fn new(child: process::ManagedChild) -> Self {
        Self(child, false)
    }

    fn stop(&mut self) {
        if self.1 {
            return;
        }
        let _ = self.0.signal(ProcessSignal::Kill);
        let _ = self.0.wait();
        self.0.join_output();
        self.1 = true;
    }
}

impl Drop for ChildTree {
    fn drop(&mut self) {
        self.stop();
    }
}

fn supervisor(lease: &Lease) -> WorkspaceSupervisor {
    WorkspaceSupervisor {
        pid: lease.pid,
        process_start_time_ticks: lease.process_start_time_ticks,
    }
}

fn associate_member(
    store: &Store,
    app: &SessionApp,
    owner: &Lease,
    child: &Lease,
) -> crate::Result<()> {
    let _operations = store.lock_operations()?;
    let associated = store.mutate(|registry| {
        let Some(lease) = registry.leases.get_mut(&app.lease_id) else {
            return Ok(false);
        };
        if lease.id != child.id
            || lease.group_id != child.group_id
            || lease.hostname != app.hostname
            || lease.workspace_supervisor.as_ref() != Some(&supervisor(owner))
            || process::lease_liveness(owner) != process::Liveness::Alive
        {
            return Ok(false);
        }
        lease.pid = child.pid;
        lease.pgid = child.pgid;
        lease.process_start_time_ticks = child.process_start_time_ticks;
        lease.state = LeaseState::Starting;
        Ok(true)
    })?;
    if !associated {
        return Err(invalid(
            "workspace lease was replaced or its supervisor is no longer alive",
        ));
    }
    Ok(())
}

pub(crate) fn run_member(arguments: crate::cli::RunArgs) -> crate::Result<i32> {
    if arguments.runner || arguments.only.is_some() {
        return Err(invalid(
            "workspace members cannot use --runner or --only; use the runner's native filters",
        ));
    }
    let marker = std::env::var_os(SESSION_VARIABLE)
        .ok_or_else(|| invalid("workspace session marker is missing"))?;
    let mut member = SessionMember::claim(
        std::path::Path::new(&marker),
        &std::env::current_dir()?,
        &arguments,
    )?;
    let config = member.config(&arguments)?;
    let mut signals = ForwardedSignals::new()?;
    member.wait_for_release()?;
    let port = member.app().port;
    let argv = process::substitute_port(&config.command, port);
    let framework = config.framework.resolve(&argv);
    let argv = match framework {
        Some(framework) => framework.inject_argv(
            argv,
            port,
            config.bind_address,
            &config.hostname,
            config.strict_port,
        ),
        None => argv,
    };
    let mut environment = member.app().environment.clone();
    environment.retain(|(key, _)| key != SESSION_VARIABLE);
    environment.extend(process::child_environment(
        port,
        config.bind_address,
        &config.hostname,
        config.tls,
    ));
    if let Some(framework) = framework {
        environment.extend(framework.environment(port, config.bind_address, &config.hostname));
    }
    let mut child = ChildTree::new(member.spawn_child(
        || {
            Ok(process::spawn_managed_child_in(
                &argv,
                &environment,
                member.app().lease_id,
                Some(&member.app().directory),
                None,
            )
            .map_err(process::RunError::from)?)
        },
        associate_member,
    )?);
    let result = (|| -> crate::Result<i32> {
        let store = Store::new(member.state_path().to_owned());
        loop {
            if let Some(code) = child.0.try_wait().map_err(process::RunError::from)? {
                return Ok(code);
            }
            if let Some(signal) = signals.pending().first() {
                let _ = child.0.signal_foreground(*signal);
                return Ok(if *signal == ProcessSignal::Interrupt {
                    130
                } else {
                    143
                });
            }
            // Recheck the context as well as the root identity: closing a live
            // root session must also end independent member process groups.
            member.check_live()?;
            let owned = store
                .load()?
                .leases
                .get(&member.app().lease_id)
                .is_some_and(|lease| {
                    lease.group_id == Some(member.group_id())
                        && lease.workspace_supervisor.as_ref()
                            == Some(&supervisor(member.supervisor()))
                        && lease.pid == child.0.pid
                        && lease.process_start_time_ticks == child.0.start_time_ticks
                });
            if !owned {
                return Err(invalid("workspace application ownership was replaced"));
            }
            thread::sleep(Duration::from_millis(20));
        }
    })();
    drop(child);
    // Root teardown may already have removed the context. Child cleanup is the
    // important ordering boundary; a missing context cannot retain a live app.
    let _ = member.finish();
    result
}

pub(crate) fn prepare(
    mut workspace: ResolvedWorkspaceConfig,
    only: Option<&str>,
) -> crate::Result<PreparedWorkspace> {
    if let Some(id) = only {
        if !workspace.apps.iter().any(|app| app.id == id) {
            return Err(invalid(format!("unknown workspace application `{id}`")));
        }
        workspace.apps.retain(|app| app.id == id);
    }
    let signals = ForwardedSignals::new()?;
    let mut apps = Vec::new();
    let mut values = BTreeMap::new();
    for app in workspace.apps {
        let reservation = process::reserve_port_on(
            app.config.bind_address,
            app.config.app_port,
            app.config.strict_port,
        )
        .map_err(process::RunError::from)?;
        let port = reservation.port;
        let address = process::readiness_probe_address(app.config.bind_address);
        values.insert(
            format!("{}.url", app.id),
            format!(
                "{}://{}",
                if app.config.tls { "https" } else { "http" },
                app.config.hostname
            ),
        );
        values.insert(
            format!("{}.upstream", app.id),
            format!("http://{}", SocketAddr::new(address, port)),
        );
        values.insert(
            format!("{}.host", app.id),
            app.config.bind_address.to_string(),
        );
        values.insert(format!("{}.port", app.id), port.to_string());
        apps.push(PreparedApp {
            app,
            port,
            environment: Vec::new(),
            reservation: Some(reservation),
        });
    }
    for prepared in &mut apps {
        for argument in &mut prepared.app.config.command {
            *argument = interpolate(argument, &values)?;
        }
        prepared.environment = prepared
            .app
            .environment
            .iter()
            .map(|(key, value)| {
                Ok((
                    OsString::from(key),
                    interpolate(OsStr::new(value), &values)?,
                ))
            })
            .collect::<crate::Result<_>>()?;
    }
    Ok(PreparedWorkspace {
        root: workspace.root,
        group_id: Uuid::new_v4(),
        apps,
        signals,
    })
}

fn interpolate(value: &OsStr, values: &BTreeMap<String, String>) -> crate::Result<OsString> {
    let text = value.to_string_lossy();
    let mut rest = text.as_ref();
    while let Some((_, after)) = rest.split_once("${") {
        let Some((reference, after)) = after.split_once('}') else {
            return Err(invalid(
                "unterminated workspace interpolation; expected ${id.url}, ${id.upstream}, ${id.host}, or ${id.port}",
            ));
        };
        if !values.contains_key(reference) {
            return Err(invalid(format!(
                "unknown workspace reference `${{{reference}}}`; references must name a selected application and url, upstream, host, or port"
            )));
        }
        rest = after;
    }
    Ok(process::substitute_values(value, values))
}

pub(crate) struct RunningWorkspace {
    pub(crate) apps: Vec<(String, process::RunningChild)>,
    clients: Vec<Option<crate::tailscale::Client>>,
    readiness: Vec<(Duration, bool, bool)>,
    signals: ForwardedSignals,
    terminated: bool,
}

struct RunnerTree {
    owner: SessionOwner,
    child: ChildTree,
    store: Store,
    group_id: Uuid,
    supervisor: WorkspaceSupervisor,
    stopped: bool,
}

impl RunnerTree {
    fn stop(&mut self) -> Vec<String> {
        if self.stopped {
            return Vec::new();
        }
        let mut warnings = Vec::new();
        let members = match self.owner.close() {
            Ok(members) => members,
            Err(error) => {
                warnings.push(format!("cannot close workspace session: {error}"));
                self.owner.members().unwrap_or_default()
            }
        };
        let mut children: BTreeMap<_, _> = members
            .into_iter()
            .filter_map(|member| {
                member
                    .child
                    .map(|child| ((child.pid, child.process_start_time_ticks), child))
            })
            .collect();
        // Association is durable before context registration. Include registry
        // children if the wrapper failed between those two writes.
        match self.store.load() {
            Ok(registry) => {
                for lease in registry.leases.into_values().filter(|lease| {
                    lease.group_id == Some(self.group_id)
                        && lease.workspace_supervisor.as_ref() == Some(&self.supervisor)
                        && lease.pid != self.child.0.pid
                }) {
                    children.insert((lease.pid, lease.process_start_time_ticks), lease);
                }
            }
            Err(error) => warnings.push(format!("cannot read workspace children: {error}")),
        }
        for child in children.values() {
            if let Err(error) = process::kill_owned_lease(child) {
                warnings.push(format!(
                    "cannot stop workspace child {}: {error}",
                    child.pid
                ));
            }
        }
        let deadline = Instant::now() + Duration::from_secs(2);
        while children
            .values()
            .any(|child| process::lease_liveness(child) == process::Liveness::Alive)
            && Instant::now() < deadline
        {
            thread::sleep(Duration::from_millis(10));
        }
        self.child.stop();
        self.stopped = true;
        warnings
    }
}

impl Drop for RunnerTree {
    fn drop(&mut self) {
        self.stop();
    }
}

fn cleanup_runner_app(
    store: &Store,
    routes: &mut impl RouteBackend,
    config: &crate::config::ResolvedRunConfig,
    owner_id: Uuid,
) -> crate::Result<Vec<String>> {
    let _operations = store.lock_operations()?;
    let cleanup = routes.remove_if_owned(&config.hostname, owner_id, config.tls);
    store.mutate(|registry| {
        registry.leases.remove(&owner_id);
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
    Ok(cleanup
        .err()
        .map(|error| {
            format!(
                "cleanup of {} is pending: {error}; run `nook prune` to retry",
                config.hostname
            )
        })
        .into_iter()
        .collect())
}

impl PreparedWorkspace {
    pub(crate) fn run_runner(
        mut self,
        command: &[OsString],
        store: &Store,
        routes: &mut impl RouteBackend,
        mut warn: impl FnMut(&str),
    ) -> crate::Result<RunOutcome> {
        if command.is_empty() {
            return Err(invalid("--runner requires a root dev command after --"));
        }
        let identity = process::read_process_identity(std::process::id())
            .ok_or_else(|| invalid("cannot identify workspace supervisor"))?;
        let root_supervisor = WorkspaceSupervisor {
            pid: std::process::id(),
            process_start_time_ticks: identity.start_time_ticks,
        };
        let operations = store.lock_operations()?;
        let mut prepared_runs = Vec::new();
        let mut runner = None;
        let mut committed = false;
        let startup = (|| -> crate::Result<Vec<String>> {
            for app in &mut self.apps {
                prepared_runs.push(process::prepare_run(
                    &app.app.config,
                    app.reservation.take().expect("reserved once"),
                    store,
                    routes,
                    &operations,
                    &mut process::LocalOnly,
                )?);
            }
            let apps = self
                .apps
                .iter()
                .zip(&prepared_runs)
                .map(|(app, run)| {
                    let config = &app.app.config;
                    SessionApp {
                        id: app.app.id.clone(),
                        directory: app.app.directory.clone(),
                        lease_id: run.owner_id,
                        hostname: config.hostname.clone(),
                        port: app.port,
                        bind_address: config.bind_address,
                        tls: config.tls,
                        strict_port: config.strict_port,
                        readiness_warn_after_seconds: config.readiness_warn_after_seconds,
                        framework: match config.framework {
                            crate::framework::FrameworkChoice::Auto => "auto",
                            crate::framework::FrameworkChoice::Disabled => "none",
                            crate::framework::FrameworkChoice::Forced(
                                crate::framework::Framework::Vite,
                            ) => "vite",
                            crate::framework::FrameworkChoice::Forced(
                                crate::framework::Framework::Nuxt,
                            ) => "nuxt",
                            crate::framework::FrameworkChoice::Forced(
                                crate::framework::Framework::Astro,
                            ) => "astro",
                        }
                        .into(),
                        environment: app
                            .environment
                            .iter()
                            .chain(&run.exposed)
                            .cloned()
                            .collect(),
                    }
                })
                .collect();
            let owner = SessionOwner::create(
                self.root.clone(),
                self.group_id,
                store.path().to_owned(),
                apps,
            )?;
            if !self.signals.pending().is_empty() {
                return Err(invalid("workspace startup interrupted"));
            }
            let child = ChildTree::new(
                process::spawn_managed_child_in(
                    command,
                    &[(SESSION_VARIABLE.into(), owner.path().as_os_str().to_owned())],
                    self.group_id,
                    Some(&self.root),
                    None,
                )
                .map_err(process::RunError::from)?,
            );
            runner = Some(RunnerTree {
                owner,
                child,
                store: Store::new(store.path().to_owned()),
                group_id: self.group_id,
                supervisor: root_supervisor.clone(),
                stopped: false,
            });
            let entries: Vec<_> = self
                .apps
                .iter()
                .zip(&prepared_runs)
                .map(|(app, run)| {
                    (
                        &app.app.config,
                        run,
                        &runner.as_ref().expect("spawned").child.0,
                    )
                })
                .collect();
            let mut warnings =
                process::commit_prepared_runs(&entries, Some(self.group_id), store, routes)?;
            committed = true;
            store.mutate(|registry| {
                for run in &prepared_runs {
                    if let Some(lease) = registry.leases.get_mut(&run.owner_id) {
                        lease.workspace_supervisor = Some(root_supervisor.clone());
                    }
                }
                Ok(())
            })?;
            warnings.extend(
                prepared_runs
                    .iter()
                    .flat_map(|run| run.warnings.iter().cloned()),
            );
            Ok(warnings)
        })();
        if !committed {
            drop(runner);
            for (app, run) in self.apps.iter().zip(&prepared_runs).rev() {
                let _ = process::abandon_prepared_run(&app.app.config, run, store, routes);
            }
            return Err(startup.expect_err("uncommitted startup failed"));
        }
        // Session callbacks acquire the store lock. Never enter them while an
        // operations lock is held, including during shutdown.
        drop(operations);
        let mut runner = runner.expect("committed runner");
        let started = Instant::now();
        let mut readiness = vec![(None, false, false); self.apps.len()];
        let result = (|| -> crate::Result<i32> {
            for warning in startup? {
                warn(&warning);
            }
            loop {
                if let Some(signal) = self.signals.pending().first() {
                    let _ = runner.child.0.signal_foreground(*signal);
                    return Ok(if *signal == ProcessSignal::Interrupt {
                        130
                    } else {
                        143
                    });
                }
                if let Some(code) = runner.child.0.try_wait().map_err(process::RunError::from)? {
                    return Ok(code);
                }
                runner.owner.poll_joins(|app, _wrapper| {
                    let registry = store.load()?;
                    if !registry.leases.get(&app.lease_id).is_some_and(|lease| {
                        lease.group_id == Some(self.group_id)
                            && lease.workspace_supervisor.as_ref() == Some(&root_supervisor)
                    }) {
                        return Err(invalid(format!("workspace app `{}` was replaced", app.id)));
                    }
                    let run = prepared_runs
                        .iter_mut()
                        .find(|run| run.owner_id == app.lease_id)
                        .ok_or_else(|| invalid("join references an unknown app lease"))?;
                    run.release_port();
                    Ok(())
                })?;
                let members = runner.owner.members()?;
                for (index, (app, run)) in self.apps.iter().zip(&prepared_runs).enumerate() {
                    let member = members.iter().find(|member| member.app_id == app.app.id);
                    let (token, ready, warned) = &mut readiness[index];
                    let current_token = member.map(|member| member.token);
                    if *token != current_token {
                        *token = current_token;
                        *ready = false;
                    }
                    let child = member.and_then(|member| member.child.as_ref());
                    let mut desired = LeaseState::Starting;
                    if let Some(child) = child {
                        let alive = process::lease_liveness(child) == process::Liveness::Alive;
                        *ready = alive
                            && std::net::TcpStream::connect_timeout(
                                &SocketAddr::new(
                                    process::readiness_probe_address(app.app.config.bind_address),
                                    app.port,
                                ),
                                Duration::from_millis(5),
                            )
                            .is_ok();
                        if *ready {
                            desired = LeaseState::Ready;
                        }
                    }
                    let needs_update = store
                        .load()?
                        .leases
                        .get(&run.owner_id)
                        .is_some_and(|lease| lease.state != desired);
                    if needs_update {
                        store.mutate(|registry| {
                            if let Some(lease) = registry.leases.get_mut(&run.owner_id)
                                && lease.group_id == Some(self.group_id)
                                && lease.workspace_supervisor.as_ref() == Some(&root_supervisor)
                                && child.is_none_or(|child| {
                                    lease.pid == child.pid
                                        && lease.process_start_time_ticks
                                            == child.process_start_time_ticks
                                })
                            {
                                lease.state = desired;
                            }
                            Ok(())
                        })?;
                    }
                    if !*ready
                        && !*warned
                        && started.elapsed()
                            >= Duration::from_secs(app.app.config.readiness_warn_after_seconds)
                    {
                        let status = if child.is_some() {
                            "is still not accepting connections"
                        } else {
                            "is waiting for the runner to start its nook-run script"
                        };
                        warn(&format!(
                            "[{}] {} {status} on port {}",
                            app.app.id, app.app.config.hostname, app.port
                        ));
                        *warned = true;
                    }
                }
                thread::sleep(Duration::from_millis(20));
            }
        })();
        let mut warnings = runner.stop();
        let mut cleanup_error = None;
        for (app, run) in self.apps.iter().zip(&prepared_runs) {
            match cleanup_runner_app(store, routes, &app.app.config, run.owner_id) {
                Ok(pending) => warnings.extend(pending),
                Err(error) => {
                    cleanup_error.get_or_insert(error);
                }
            }
        }
        if let Some(error) = cleanup_error {
            return Err(error);
        }
        Ok(RunOutcome {
            exit_code: result?,
            warnings,
        })
    }

    /// All routes are installed while every port remains reserved; only then spawn.
    pub(crate) fn start(
        mut self,
        store: &Store,
        routes: &mut impl RouteBackend,
    ) -> crate::Result<RunningWorkspace> {
        let mut clients = Vec::new();
        for prepared in &self.apps {
            let config = &prepared.app.config;
            let client = config
                .tailscale
                .then(|| crate::tailscale::Client::from_environment().with_funnel(config.funnel));
            if let Some(client) = &client {
                crate::tailscale::reachable_bind_address(config.bind_address)?;
                client.require_ready()?;
            }
            clients.push(client);
        }
        let operations = store.lock_operations()?;
        let mut prepared_runs = Vec::new();
        let mut running = Vec::new();
        let result = (|| -> crate::Result<()> {
            for (index, prepared) in self.apps.iter_mut().enumerate() {
                let config = &prepared.app.config;
                let reservation = prepared.reservation.take().expect("reserved once");
                let run = if let Some(client) = &clients[index] {
                    process::prepare_run(
                        config,
                        reservation,
                        store,
                        routes,
                        &operations,
                        &mut crate::tailscale::ServeExposure::new(client),
                    )?
                } else {
                    process::prepare_run(
                        config,
                        reservation,
                        store,
                        routes,
                        &operations,
                        &mut process::LocalOnly,
                    )?
                };
                prepared_runs.push(run);
            }
            for (prepared, run) in self.apps.iter().zip(&mut prepared_runs) {
                if !self.signals.pending().is_empty() {
                    return Err(io::Error::new(
                        io::ErrorKind::Interrupted,
                        "workspace startup interrupted",
                    )
                    .into());
                }
                let child = process::spawn_prepared_run(
                    &prepared.app.config,
                    run,
                    Some(&prepared.app.directory),
                    Some(&prepared.app.id),
                    &prepared.environment,
                    |_| {},
                )?;
                running.push((prepared.app.id.clone(), child));
            }
            let entries: Vec<_> = self
                .apps
                .iter()
                .zip(&prepared_runs)
                .zip(&running)
                .map(|((app, prepared), (_, running))| (&app.app.config, prepared, &running.child))
                .collect();
            let warnings =
                process::commit_prepared_runs(&entries, Some(self.group_id), store, routes)?;
            if !warnings.is_empty()
                && let Some((_, app)) = running.first_mut()
            {
                let warning = app.warning.get_or_insert_with(String::new);
                if !warning.is_empty() {
                    warning.push_str("; ");
                }
                warning.push_str(&warnings.join("; "));
            }
            Ok(())
        })();
        if let Err(error) = result {
            for (_, running) in &mut running {
                let _ = running.child.signal(ProcessSignal::Kill);
                let _ = running.child.wait();
                running.child.join_output();
            }
            for (index, run) in prepared_runs.iter().enumerate().rev() {
                if let Some(client) = &clients[index] {
                    crate::tailscale::ServeExposure::new(client).withdraw(
                        &operations,
                        store,
                        run.owner_id,
                    );
                }
                let _ =
                    process::abandon_prepared_run(&self.apps[index].app.config, run, store, routes);
            }
            return Err(error);
        }
        let readiness = self
            .apps
            .iter()
            .map(|app| {
                (
                    Duration::from_secs(app.app.config.readiness_warn_after_seconds),
                    false,
                    false,
                )
            })
            .collect();
        Ok(RunningWorkspace {
            apps: running,
            clients,
            readiness,
            signals: self.signals,
            terminated: false,
        })
    }
}

impl RunningWorkspace {
    /// Watch every app together, including while other apps are still starting.
    /// The first exit or foreground signal ends the session and stops the rest.
    pub(crate) fn finish(
        &mut self,
        store: &Store,
        routes: &mut impl RouteBackend,
        mut warn: impl FnMut(&str),
    ) -> crate::Result<RunOutcome> {
        let started = Instant::now();
        let result = (|| -> crate::Result<i32> {
            loop {
                if let Some(signal) = self.signals.pending().first() {
                    for (_, running) in &self.apps {
                        let _ = running.child.signal_foreground(*signal);
                    }
                    return Ok(if *signal == ProcessSignal::Interrupt {
                        130
                    } else {
                        143
                    });
                }
                for (index, (id, running)) in self.apps.iter_mut().enumerate() {
                    if let Some(code) = running.child.try_wait().map_err(process::RunError::from)? {
                        return Ok(code);
                    }
                    let (after, ready, warned) = &mut self.readiness[index];
                    if !*ready {
                        *ready = running.poll_readiness(store)?;
                        if !*ready && !*warned && started.elapsed() >= *after {
                            warn(&format!(
                                "[{id}] {} is still not accepting connections on port {}; its route and process remain active (check that it binds HOST and PORT)",
                                running.hostname, running.port
                            ));
                            *warned = true;
                        }
                    }
                }
                thread::sleep(Duration::from_millis(20));
            }
        })();
        self.terminate();
        let mut warnings = Vec::new();
        let mut cleanup_error = None;
        for (_, running) in &mut self.apps {
            match running.cleanup(store, routes) {
                Ok(pending) => warnings.extend(pending),
                Err(error) => {
                    cleanup_error.get_or_insert(crate::Error::from(error));
                }
            }
        }
        // Also withdraw Serve registrations on error, without needing a daemon.
        if self.clients.iter().any(Option::is_some) {
            let operations = store.lock_operations()?;
            for (client, (_, running)) in self.clients.iter().zip(&self.apps) {
                if let Some(client) = client {
                    crate::tailscale::ServeExposure::new(client).withdraw(
                        &operations,
                        store,
                        running.lease_id,
                    );
                }
            }
        }
        if let Some(error) = cleanup_error {
            return Err(error);
        }
        Ok(RunOutcome {
            exit_code: result?,
            warnings,
        })
    }

    fn terminate(&mut self) {
        if self.terminated {
            return;
        }
        self.terminated = true;
        for (_, running) in &self.apps {
            let _ = running.child.signal_foreground(ProcessSignal::Terminate);
        }
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline {
            if self
                .apps
                .iter_mut()
                .all(|(_, app)| app.child.try_wait().ok().flatten().is_some())
            {
                break;
            }
            if !self.signals.pending().is_empty() {
                break;
            }
            thread::sleep(Duration::from_millis(20));
        }
        // Kill groups even after the leader exits: descendants may retain pipes.
        for (_, running) in &mut self.apps {
            let _ = running.child.signal(ProcessSignal::Kill);
            let _ = running.child.wait();
            running.child.join_output();
        }
    }
}

impl Drop for RunningWorkspace {
    fn drop(&mut self) {
        // Errors in terminal output or readiness must never orphan a process tree.
        if self.terminated {
            return;
        }
        for (_, running) in &mut self.apps {
            let _ = running.child.signal(ProcessSignal::Kill);
            let _ = running.child.wait();
            running.child.join_output();
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::config::ResolvedRunConfig;
    use crate::reconcile::{RouteError, RouteSpec};
    use crate::state::{Alias, Scheme};
    use std::fs;
    use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, TcpListener};
    use std::os::unix::ffi::{OsStrExt, OsStringExt};

    struct Fixture {
        root: PathBuf,
        store: Store,
    }
    impl Fixture {
        fn new() -> Self {
            let root = std::env::temp_dir().join(format!("nook-workspace-{}", Uuid::new_v4()));
            for id in ["web", "api"] {
                fs::create_dir_all(root.join(id)).unwrap();
            }
            Self {
                store: Store::new(root.join("state.json")),
                root,
            }
        }
        fn workspace(&self, commands: [&[&str]; 2]) -> ResolvedWorkspaceConfig {
            ResolvedWorkspaceConfig {
                root: self.root.clone(),
                apps: ["web", "api"]
                    .into_iter()
                    .zip(commands)
                    .map(|(id, argv)| WorkspaceApp {
                        id: id.into(),
                        directory: self.root.join(id),
                        environment: BTreeMap::new(),
                        config: ResolvedRunConfig {
                            hostname: format!("{id}.demo.localhost"),
                            command: argv.iter().map(OsString::from).collect(),
                            tls: true,
                            tailscale: false,
                            funnel: false,
                            lan: false,
                            app_port: None,
                            strict_port: false,
                            force: false,
                            readiness_warn_after_seconds: 30,
                            bind_address: Ipv4Addr::LOCALHOST.into(),
                            ignored_local_config: None,
                            framework: crate::framework::FrameworkChoice::Disabled,
                        },
                    })
                    .collect(),
            }
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    #[derive(Default)]
    struct Routes {
        owners: BTreeMap<String, Uuid>,
        marker: Option<PathBuf>,
    }
    impl RouteBackend for Routes {
        fn ensure(&mut self, route: &RouteSpec) -> Result<(), RouteError> {
            if let Some(marker) = &self.marker {
                assert!(
                    !marker.exists(),
                    "a child started before all routes were installed"
                );
            }
            self.owners.insert(route.hostname.clone(), route.owner_id);
            Ok(())
        }
        fn remove_if_owned(
            &mut self,
            hostname: &str,
            owner: Uuid,
            _: bool,
        ) -> Result<(), RouteError> {
            if self.owners.get(hostname) == Some(&owner) {
                self.owners.remove(hostname);
            }
            Ok(())
        }
    }

    #[test]
    fn plan_uses_reserved_fallback_ports_for_argv_and_environment() {
        let fixture = Fixture::new();
        let occupied = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let mut config = fixture.workspace([
            &[
                "echo",
                "${api.url}",
                "${api.upstream}",
                "${api.host}:${api.port}",
            ],
            &["echo"],
        ]);
        config.apps[1].config.app_port = Some(occupied.local_addr().unwrap().port());
        config.apps[0]
            .environment
            .insert("API".into(), "${api.upstream}".into());
        config.apps[0]
            .environment
            .insert("PORT".into(), "wrong".into());
        let plan = prepare(config, None).unwrap();
        let port = plan.apps[1].port;
        assert_ne!(port, occupied.local_addr().unwrap().port());
        assert_eq!(
            plan.apps[0].app.config.command[1],
            "https://api.demo.localhost"
        );
        assert_eq!(
            plan.apps[0].app.config.command[2].to_string_lossy(),
            format!("http://127.0.0.1:{port}")
        );
        assert_eq!(
            plan.apps[0].app.config.command[3].to_string_lossy(),
            format!("127.0.0.1:{port}")
        );
        assert_eq!(
            plan.apps[0].environment[0].1.to_string_lossy(),
            format!("http://127.0.0.1:{port}")
        );
        assert!(TcpListener::bind((Ipv4Addr::LOCALHOST, port)).is_err());
        assert_ne!(plan.apps[0].port, port);
    }

    #[test]
    fn wildcard_upstream_uses_loopback_and_ipv6_is_bracketed() {
        let fixture = Fixture::new();
        let mut config =
            fixture.workspace([&["echo", "${api.upstream}", "${api.host}"], &["echo"]]);
        config.apps[1].config.bind_address = Ipv4Addr::UNSPECIFIED.into();
        let plan = prepare(config, None).unwrap();
        assert_eq!(
            plan.apps[0].app.config.command[1].to_string_lossy(),
            format!("http://127.0.0.1:{}", plan.apps[1].port)
        );
        assert_eq!(plan.apps[0].app.config.command[2], "0.0.0.0");
        let mut config = fixture.workspace([&["echo", "${api.upstream}"], &["echo"]]);
        config.apps[1].config.bind_address = IpAddr::V6(Ipv6Addr::LOCALHOST);
        let plan = prepare(config, None).unwrap();
        assert_eq!(
            plan.apps[0].app.config.command[1].to_string_lossy(),
            format!("http://[::1]:{}", plan.apps[1].port)
        );
    }

    #[test]
    fn only_selects_one_app_and_reports_unknown_or_excluded_references() {
        let fixture = Fixture::new();
        assert_eq!(
            prepare(fixture.workspace([&["echo"], &["echo"]]), Some("api"))
                .unwrap()
                .apps
                .len(),
            1
        );
        let error = prepare(fixture.workspace([&["echo"], &["echo"]]), Some("missing"))
            .err()
            .unwrap();
        assert!(
            error
                .to_string()
                .contains("unknown workspace application `missing`")
        );
        let error = prepare(
            fixture.workspace([&["echo", "${api.url}"], &["echo"]]),
            Some("web"),
        )
        .err()
        .unwrap();
        assert!(error.to_string().contains("selected application"));
        assert!(error.to_string().contains("${api.url}"));
    }

    #[test]
    fn interpolation_preserves_non_unicode_argv_and_rejects_malformed_references() {
        let values = BTreeMap::from([("api.port".into(), "4321".into())]);
        let raw = OsString::from_vec(b"\x80-${api.port}".to_vec());
        assert_eq!(interpolate(&raw, &values).unwrap().as_bytes(), b"\x80-4321");
        assert!(interpolate(OsStr::new("${api.port"), &values).is_err());
        assert!(interpolate(OsStr::new("${api.nope}"), &values).is_err());
    }

    #[test]
    fn strict_port_conflict_prevents_any_startup() {
        let fixture = Fixture::new();
        let occupied = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let mut config = fixture.workspace([&["echo"], &["echo"]]);
        config.apps[1].config.app_port = Some(occupied.local_addr().unwrap().port());
        config.apps[1].config.strict_port = true;
        assert!(prepare(config, None).is_err());
        assert!(fixture.store.load().unwrap().pending_operations.is_empty());
    }

    #[test]
    fn all_routes_precede_spawn_and_child_gets_its_directory_and_actual_environment() {
        let fixture = Fixture::new();
        let marker = fixture.root.join("web/started");
        let mut config = fixture.workspace([
            &["/usr/bin/python3", "-c", r"import os,sys,time;open('started','w').write(os.getcwd()+'\n'+os.environ['API']+'\n'+os.environ['PORT']+'\n'+sys.argv[1]);time.sleep(.1)", "${api.port}"],
            &["/bin/sleep", "30"],
        ]);
        config.apps[0]
            .environment
            .insert("API".into(), "${api.upstream}".into());
        config.apps[0]
            .environment
            .insert("PORT".into(), "incorrect".into());
        let plan = prepare(config, None).unwrap();
        let group = plan.group_id;
        let web_port = plan.apps[0].port;
        let api_port = plan.apps[1].port;
        let mut routes = Routes {
            marker: Some(marker.clone()),
            ..Routes::default()
        };
        let mut running = plan.start(&fixture.store, &mut routes).unwrap();
        let leases = fixture.store.load().unwrap().leases;
        assert_eq!(leases.len(), 2);
        assert!(leases.values().all(|lease| lease.group_id == Some(group)));
        assert_eq!(
            running
                .finish(&fixture.store, &mut routes, |_| {})
                .unwrap()
                .exit_code,
            0
        );
        let contents = fs::read_to_string(marker).unwrap();
        assert_eq!(
            contents,
            format!(
                "{}\nhttp://127.0.0.1:{api_port}\n{web_port}\n{api_port}",
                fixture.root.join("web").display()
            )
        );
        assert!(fixture.store.load().unwrap().leases.is_empty());
        assert!(routes.owners.is_empty());
        assert!(
            leases
                .values()
                .all(|lease| process::lease_liveness(lease) == process::Liveness::Dead)
        );
    }

    #[test]
    fn partial_spawn_failure_restores_forced_alias_and_stops_started_group() {
        let fixture = Fixture::new();
        let mut config = fixture.workspace([
            &["/bin/sh", "-c", "sleep .2; touch escaped; sleep 30"],
            &["/nonexistent/nook-workspace-app"],
        ]);
        config.apps[0].config.force = true;
        let alias = Alias {
            id: Uuid::new_v4(),
            hostname: "web.demo.localhost".into(),
            target: "http://127.0.0.1:3000".into(),
            scheme: Scheme::Http,
            tls: true,
            preserve_host: false,
        };
        fixture
            .store
            .mutate(|registry| {
                registry
                    .aliases
                    .insert(alias.hostname.clone(), alias.clone());
                Ok(())
            })
            .unwrap();
        let mut routes = Routes {
            owners: BTreeMap::from([(alias.hostname.clone(), alias.id)]),
            ..Routes::default()
        };
        assert!(
            prepare(config, None)
                .unwrap()
                .start(&fixture.store, &mut routes)
                .is_err()
        );
        let registry = fixture.store.load().unwrap();
        assert_eq!(registry.aliases[&alias.hostname], alias);
        assert_eq!(
            routes.owners,
            BTreeMap::from([(alias.hostname.clone(), alias.id)])
        );
        assert!(registry.leases.is_empty());
        assert!(registry.pending_operations.is_empty());
        thread::sleep(Duration::from_millis(300));
        assert!(!fixture.root.join("web/escaped").exists());
    }

    #[test]
    fn nonzero_exit_stops_other_apps_and_cleans_every_route_and_lease() {
        let fixture = Fixture::new();
        let config = fixture.workspace([
            &["/bin/sh", "-c", "sleep .1; exit 7"],
            &["/bin/sleep", "30"],
        ]);
        let mut routes = Routes::default();
        let mut running = prepare(config, None)
            .unwrap()
            .start(&fixture.store, &mut routes)
            .unwrap();
        let leases = fixture.store.load().unwrap().leases;
        assert_eq!(
            running
                .finish(&fixture.store, &mut routes, |_| {})
                .unwrap()
                .exit_code,
            7
        );
        assert!(
            leases
                .values()
                .all(|lease| process::lease_liveness(lease) == process::Liveness::Dead)
        );
        assert!(fixture.store.load().unwrap().leases.is_empty());
        assert!(routes.owners.is_empty());
    }

    #[test]
    fn external_runner_uses_root_argv_and_keeps_unjoined_ports_reserved() {
        let fixture = Fixture::new();
        let marker = fixture.root.join("started");
        let mut config = fixture.workspace([&["/nonexistent/app-web"], &["/nonexistent/app-api"]]);
        for app in &mut config.apps {
            app.config.readiness_warn_after_seconds = 0;
        }
        let plan = prepare(config, None).unwrap();
        let ports: Vec<_> = plan.apps.iter().map(|app| app.port).collect();
        let argv = [
            OsString::from("/usr/bin/python3"),
            OsString::from("-c"),
            OsString::from(
                r"import json,os,socket,time; p=os.environ['NOOK_WORKSPACE_SESSION']; c=json.load(open(p)); assert len(c['apps'])==2; open('started','w').write(p); time.sleep(.15); raise SystemExit(7)",
            ),
        ];
        let mut routes = Routes {
            marker: Some(marker.clone()),
            ..Routes::default()
        };
        let mut warnings = Vec::new();
        let outcome = plan
            .run_runner(&argv, &fixture.store, &mut routes, |warning| {
                // Until a matching wrapper joins, no runner app can bind its port.
                assert!(
                    ports
                        .iter()
                        .all(|port| TcpListener::bind((Ipv4Addr::LOCALHOST, *port)).is_err())
                );
                warnings.push(warning.to_owned());
            })
            .unwrap();
        assert_eq!(outcome.exit_code, 7);
        assert_eq!(warnings.len(), 2);
        assert!(
            warnings
                .iter()
                .all(|warning| warning.contains("waiting for the runner"))
        );
        let context = fs::read_to_string(marker).unwrap();
        assert!(!std::path::Path::new(&context).exists());
        assert!(fixture.store.load().unwrap().leases.is_empty());
        assert!(routes.owners.is_empty());
        assert!(
            ports
                .iter()
                .all(|port| TcpListener::bind((Ipv4Addr::LOCALHOST, *port)).is_ok())
        );
    }

    #[test]
    fn external_runner_rolls_back_forced_owner_only_before_commit() {
        for spawn_succeeds in [false, true] {
            let fixture = Fixture::new();
            let mut config = fixture.workspace([&["unused"], &["unused"]]);
            config.apps[0].config.force = true;
            let alias = Alias {
                id: Uuid::new_v4(),
                hostname: "web.demo.localhost".into(),
                target: "http://127.0.0.1:3000".into(),
                scheme: Scheme::Http,
                tls: true,
                preserve_host: false,
            };
            fixture
                .store
                .mutate(|registry| {
                    registry
                        .aliases
                        .insert(alias.hostname.clone(), alias.clone());
                    Ok(())
                })
                .unwrap();
            let mut routes = Routes {
                owners: BTreeMap::from([(alias.hostname.clone(), alias.id)]),
                ..Routes::default()
            };
            let argv = if spawn_succeeds {
                vec![
                    OsString::from("/bin/sh"),
                    "-c".into(),
                    "sleep .05; exit 0".into(),
                ]
            } else {
                vec![OsString::from("/nonexistent/nook-workspace-runner")]
            };
            let result = prepare(config, None).unwrap().run_runner(
                &argv,
                &fixture.store,
                &mut routes,
                |_| {},
            );
            assert_eq!(result.is_ok(), spawn_succeeds);
            let registry = fixture.store.load().unwrap();
            assert!(registry.leases.is_empty());
            assert!(registry.pending_operations.is_empty());
            if spawn_succeeds {
                assert!(registry.aliases.is_empty());
                assert!(routes.owners.is_empty());
            } else {
                assert_eq!(registry.aliases[&alias.hostname], alias);
                assert_eq!(routes.owners, BTreeMap::from([(alias.hostname, alias.id)]));
            }
        }
    }

    #[test]
    fn member_association_rejects_replaced_supervisors_groups_and_owners() {
        let fixture = Fixture::new();
        let group_id = Uuid::new_v4();
        let lease_id = Uuid::new_v4();
        let identity = process::read_process_identity(std::process::id()).unwrap();
        let owner = Lease {
            group_id: Some(group_id),
            workspace_supervisor: None,
            lan: false,
            id: group_id,
            hostname: String::new(),
            target: String::new(),
            scheme: Scheme::Http,
            tls: true,
            pid: std::process::id(),
            pgid: identity.pgid,
            process_start_time_ticks: identity.start_time_ticks,
            state: LeaseState::Starting,
        };
        let app = SessionApp {
            id: "web".into(),
            directory: fixture.root.join("web"),
            lease_id,
            hostname: "web.demo.localhost".into(),
            port: 3000,
            bind_address: Ipv4Addr::LOCALHOST.into(),
            tls: true,
            strict_port: false,
            readiness_warn_after_seconds: 30,
            framework: "none".into(),
            environment: Vec::new(),
        };
        let child = ChildTree::new(
            process::spawn_managed_child_in(
                &["/bin/sleep".into(), "30".into()],
                &[],
                lease_id,
                None,
                None,
            )
            .unwrap(),
        );
        let child_lease = Lease {
            id: lease_id,
            hostname: app.hostname.clone(),
            pid: child.0.pid,
            pgid: child.0.pgid,
            process_start_time_ticks: child.0.start_time_ticks,
            workspace_supervisor: Some(supervisor(&owner)),
            ..owner.clone()
        };
        let mut placeholder = child_lease.clone();
        placeholder.pid = owner.pid;
        placeholder.pgid = owner.pgid;
        placeholder.process_start_time_ticks = owner.process_start_time_ticks;
        placeholder.workspace_supervisor = None;
        fixture
            .store
            .mutate(|registry| {
                registry.leases.insert(lease_id, placeholder);
                Ok(())
            })
            .unwrap();
        assert!(associate_member(&fixture.store, &app, &owner, &child_lease).is_err());
        fixture
            .store
            .mutate(|registry| {
                let lease = registry.leases.get_mut(&lease_id).unwrap();
                lease.workspace_supervisor = Some(supervisor(&owner));
                lease.group_id = Some(Uuid::new_v4());
                Ok(())
            })
            .unwrap();
        assert!(associate_member(&fixture.store, &app, &owner, &child_lease).is_err());
        fixture
            .store
            .mutate(|registry| {
                registry.leases.get_mut(&lease_id).unwrap().group_id = Some(group_id);
                Ok(())
            })
            .unwrap();
        associate_member(&fixture.store, &app, &owner, &child_lease).unwrap();
        assert_eq!(
            fixture.store.load().unwrap().leases[&lease_id].pid,
            child.0.pid
        );
        fixture
            .store
            .mutate(|registry| {
                registry.leases.remove(&lease_id);
                Ok(())
            })
            .unwrap();
        assert!(associate_member(&fixture.store, &app, &owner, &child_lease).is_err());
    }
}
