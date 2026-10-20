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
use crate::state::Store;
use crate::workspace_config::{ResolvedWorkspaceConfig, WorkspaceApp};

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

impl PreparedWorkspace {
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
}
