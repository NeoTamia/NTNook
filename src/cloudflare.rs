//! Owned, foreground Quick Tunnels. No account provisioning or service management.
use std::ffi::OsString;
use std::fs;
use std::io;
use std::net::{Ipv4Addr, SocketAddr, TcpListener};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::process::{self, ForwardedSignals, Liveness, ManagedChild, ProcessSignal, RunError};
use crate::reconcile::{RouteBackend, RouteSpec};
use crate::state::{Lease, LeaseState, Registry, Scheme, Store, WorkspaceSupervisor};

pub(crate) const URL_VARIABLE: &str = "NOOK_TUNNEL_URL";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct Settings {
    pub(crate) binary: String,
    pub(crate) protocol: String,
    pub(crate) startup_timeout_seconds: u64,
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            binary: "cloudflared".into(),
            protocol: "auto".into(),
            startup_timeout_seconds: 60,
        }
    }
}
impl Settings {
    pub(crate) fn validate(&self) -> Result<(), String> {
        if self.binary.is_empty()
            || !matches!(self.protocol.as_str(), "auto" | "quic" | "http2")
            || !(1..=600).contains(&self.startup_timeout_seconds)
        {
            return Err("expected a binary, protocol auto/quic/http2, and startup_timeout_seconds in 1..=600".into());
        }
        Ok(())
    }
}

/// Check executable capabilities before reserving any application's routes/ports.
pub(crate) fn preflight() -> Result<(), RunError> {
    let settings = crate::config::load_global().map_err(wrapped)?.cloudflare;
    for args in [
        vec!["--version"],
        vec!["tunnel", "--protocol", "auto", "--help"],
    ] {
        let argv: Vec<_> = std::iter::once(settings.binary.as_str())
            .chain(args.iter().copied())
            .map(OsString::from)
            .collect();
        let mut child = process::spawn_auxiliary(&argv, Uuid::new_v4())
            .map_err(|_| failure("cloudflared is not executable; install it explicitly or set [cloudflare].binary (tested with 2026.10.7)"))?;
        let deadline = Instant::now() + Duration::from_secs(5);
        let status = loop {
            if let Some(status) = child.try_wait()? {
                break status;
            }
            if Instant::now() >= deadline {
                let _ = child.signal(ProcessSignal::Kill);
                let _ = child.wait();
                return Err(failure("cloudflared capability check timed out"));
            }
            std::thread::sleep(Duration::from_millis(20));
        };
        child.join_output();
        let output = child
            .auxiliary_output
            .lock()
            .map_err(|_| failure("cannot read cloudflared capabilities"))?;
        let text = String::from_utf8_lossy(&output);
        let required: &[&str] = if args[0] == "--version" {
            &["cloudflared version "]
        } else {
            &[
                "--metrics",
                "--pidfile",
                "--url",
                "--origin-server-name",
                "--origin-ca-pool",
                "--http-host-header",
            ]
        };
        if status != 0 || required.iter().any(|flag| !text.contains(flag)) {
            return Err(failure(
                "unsupported cloudflared executable; install a compatible release (tested with 2026.10.7)",
            ));
        }
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Phase {
    Preparing,
    Connected,
    Degraded,
    Failed,
    Stopping,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Registration {
    pub(crate) id: Uuid,
    pub(crate) owner_id: Uuid,
    pub(crate) hostname: String,
    pub(crate) target: String,
    pub(crate) supervisor: WorkspaceSupervisor,
    pub(crate) child: Option<Lease>,
    pub(crate) url: Option<String>,
    pub(crate) metrics_port: u16,
    pub(crate) phase: Phase,
    pub(crate) committed: bool,
}
impl Registration {
    pub(crate) fn route(&self) -> RouteSpec {
        RouteSpec {
            owner_id: self.id,
            hostname: format!("tunnel-{}.localhost", self.id.simple()),
            target: self.target.clone(),
            scheme: Scheme::Http,
            tls: true,
            lan: false,
            replace_existing: false,
            preserve_host: false,
            public_host: Some(
                self.url
                    .as_deref()
                    .and_then(|value| url::Url::parse(value).ok())
                    .and_then(|value| value.host_str().map(str::to_owned))
                    .unwrap_or_default(),
            ),
        }
    }
    fn directory(&self, store: &Store) -> PathBuf {
        store.path().with_file_name(format!("tunnel-{}", self.id))
    }
}

fn failure(message: impl Into<String>) -> RunError {
    RunError::Exposure(Box::new(io::Error::other(message.into())))
}
fn wrapped(error: impl std::fmt::Display) -> RunError {
    failure(error.to_string())
}

pub(crate) struct Tunnel {
    id: Uuid,
    store: Store,
    child: Option<ManagedChild>,
    agent: ureq::Agent,
    last_poll: Instant,
    failures: u8,
}

impl Tunnel {
    // Called after journaling the ordinary run, with no operations lock held.
    pub(crate) fn start(
        config: &crate::config::ResolvedRunConfig,
        owner_id: Uuid,
        target: &str,
        store: &Store,
        routes: &mut impl RouteBackend,
    ) -> Result<(Self, String), RunError> {
        let global = crate::config::load_global().map_err(wrapped)?;
        let settings = &global.cloudflare;
        settings.validate().map_err(failure)?;
        let supervisor = process::read_process_identity(std::process::id())
            .ok_or_else(|| failure("cannot identify tunnel supervisor"))?;
        let reservation = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).map_err(wrapped)?;
        let registration = Registration {
            id: Uuid::new_v4(),
            owner_id,
            hostname: config.hostname.clone(),
            target: target.into(),
            supervisor: WorkspaceSupervisor {
                pid: std::process::id(),
                process_start_time_ticks: supervisor.start_time_ticks,
            },
            child: None,
            url: None,
            metrics_port: reservation.local_addr().map_err(wrapped)?.port(),
            phase: Phase::Preparing,
            committed: false,
        };
        let id = registration.id;
        let mut tunnel = Self {
            id,
            store: store.clone(),
            child: None,
            agent: local_agent(),
            last_poll: Instant::now(),
            failures: 0,
        };
        let result = (|| {
            let pem = routes.local_ca()?;
            let (pem, _) = crate::caddy::canonical_local_ca(&pem).map_err(wrapped)?;
            {
                let _operations = store.lock_operations()?;
                store.mutate(|registry| {
                    registry.tunnels.insert(id, registration.clone());
                    Ok(())
                })?;
                routes.ensure(&registration.route())?;
            }
            let directory = registration.directory(store);
            crate::platform::create_private_directory(&directory).map_err(wrapped)?;
            fs::write(directory.join("config.yml"), "{}\n").map_err(wrapped)?;
            fs::write(directory.join("caddy-root.pem"), &pem).map_err(wrapped)?;
            let mut signals = ForwardedSignals::new().map_err(wrapped)?;
            let deadline = Instant::now() + Duration::from_secs(settings.startup_timeout_seconds);
            let tls = origin_agent(&pem).map_err(wrapped)?;
            let origin = format!("https://{}/", registration.route().hostname);
            loop {
                tunnel.require_starting(&mut signals)?;
                if tls
                    .get(&origin)
                    .call()
                    .is_ok_and(|response| response.status() == 503)
                {
                    break;
                }
                if Instant::now() >= deadline {
                    return Err(failure(
                        "Caddy tunnel origin TLS/readiness timed out; check HTTPS :443, local CA and client IP ranges",
                    ));
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            let argv = command(settings, &registration, &directory);
            drop(reservation);
            let child = process::spawn_auxiliary(&argv, id)?;
            let lease = Lease {
                id,
                hostname: registration.route().hostname,
                target: target.into(),
                scheme: Scheme::Http,
                tls: true,
                lan: false,
                worktree: config.worktree.clone(),
                group_id: None,
                workspace_supervisor: None,
                pid: child.pid,
                pgid: child.pgid,
                process_start_time_ticks: child.start_time_ticks,
                state: LeaseState::Starting,
            };
            tunnel.child = Some(child);
            store.mutate(|registry| {
                if let Some(entry) = registry.tunnels.get_mut(&id) {
                    entry.child = Some(lease);
                }
                Ok(())
            })?;
            loop {
                tunnel.require_starting(&mut signals)?;
                if tunnel
                    .child
                    .as_mut()
                    .expect("spawned")
                    .try_wait()?
                    .is_some()
                {
                    return Err(failure(
                        "cloudflared exited before connecting; check its installation, configuration and outbound TCP/UDP 7844 (tested with 2026.10.7)",
                    ));
                }
                // cloudflared writes this only after its first edge connection,
                // after binding metrics. A foreign listener on a raced port is
                // not evidence that our child connected.
                let own_connection = fs::read_to_string(directory.join("connector.pid"))
                    .ok()
                    .is_some_and(|pid| {
                        pid.trim().parse::<u32>().ok()
                            == tunnel.child.as_ref().map(|child| child.pid)
                    });
                if own_connection
                    && let Some(url) = discover(&tunnel.agent, registration.metrics_port)?
                    && ready(&tunnel.agent, registration.metrics_port)
                {
                    let _operations = store.lock_operations()?;
                    tunnel.require_starting(&mut signals)?;
                    let mut active = registration.clone();
                    active.url = Some(url.clone());
                    routes.ensure(&active.route())?;
                    store.mutate(|registry| {
                        if let Some(entry) = registry.tunnels.get_mut(&id) {
                            entry.url = Some(url.clone());
                            entry.phase = Phase::Connected;
                        }
                        Ok(())
                    })?;
                    eprintln!(
                        "nook: domain={} tunnel_url={} tunnel=connected (public, ephemeral, no SSE)",
                        config.hostname, url
                    );
                    return Ok(url);
                }
                if Instant::now() >= deadline {
                    return Err(failure(
                        "Cloudflare startup timed out waiting for URL and connection; check DNS, HTTPS and outbound TCP/UDP 7844",
                    ));
                }
                std::thread::sleep(Duration::from_millis(250));
            }
        })();
        match result {
            Ok(url) => Ok((tunnel, url)),
            Err(error) => {
                tunnel.stop();
                if let Ok(_operations) = store.lock_operations() {
                    let _ = store.mutate(|registry| {
                        reconcile(registry, store, routes);
                        Ok(())
                    });
                }
                Err(error)
            }
        }
    }

    fn require_starting(&self, signals: &mut ForwardedSignals) -> Result<(), RunError> {
        if !signals.pending().is_empty() {
            return Err(failure("tunnel startup interrupted"));
        }
        let registry = self.store.load()?;
        if registry.tunnels.get(&self.id).is_none_or(|r| {
            r.phase == Phase::Stopping
                || registry
                    .preparing
                    .get(&r.owner_id)
                    .is_none_or(|p| p.cancelled)
        }) {
            return Err(failure("tunnel startup cancelled"));
        }
        Ok(())
    }

    pub(crate) fn verify_ready(&mut self) -> Result<(), RunError> {
        let registry = self.store.load()?;
        let entry = registry
            .tunnels
            .get(&self.id)
            .ok_or_else(|| failure("tunnel ownership disappeared"))?;
        if entry.phase == Phase::Stopping
            || self
                .child
                .as_mut()
                .is_none_or(|child| !matches!(child.try_wait(), Ok(None)))
            || !ready(&self.agent, entry.metrics_port)
        {
            return Err(failure(
                "a prepared tunnel failed before applications could start",
            ));
        }
        Ok(())
    }

    pub(crate) fn poll(&mut self) {
        if self.last_poll.elapsed() < Duration::from_secs(2) {
            return;
        }
        self.last_poll = Instant::now();
        let Ok(registry) = self.store.load() else {
            return;
        };
        let Some(entry) = registry.tunnels.get(&self.id) else {
            self.stop();
            return;
        };
        if entry.phase == Phase::Stopping
            || (entry.committed && !registry.leases.contains_key(&entry.owner_id))
        {
            self.stop();
            return;
        }
        let Some(child) = &mut self.child else {
            return;
        };
        let phase = match child.try_wait() {
            Ok(Some(_)) => Phase::Failed,
            Ok(None) if ready(&self.agent, entry.metrics_port) => {
                self.failures = 0;
                Phase::Connected
            }
            _ => {
                self.failures = self.failures.saturating_add(1);
                if self.failures < 3 {
                    return;
                }
                Phase::Degraded
            }
        };
        if phase != entry.phase {
            eprintln!(
                "nook: domain={} tunnel={phase:?}; local application remains running",
                entry.hostname
            );
            let _ = self.store.mutate(|registry| {
                if let Some(entry) = registry.tunnels.get_mut(&self.id)
                    && entry.phase != Phase::Stopping
                {
                    entry.phase = phase;
                }
                Ok(())
            });
        }
    }

    pub(crate) fn stop(&mut self) {
        let _ = self.store.mutate(|registry| {
            if let Some(entry) = registry.tunnels.get_mut(&self.id) {
                entry.phase = Phase::Stopping;
            }
            Ok(())
        });
        if let Some(mut child) = self.child.take() {
            let _ = child.signal(ProcessSignal::Terminate);
            let deadline = Instant::now() + Duration::from_secs(5);
            while matches!(child.try_wait(), Ok(None)) && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(20));
            }
            let _ = child.signal(ProcessSignal::Kill);
            let _ = child.wait();
            child.join_output();
        }
    }
}
impl Drop for Tunnel {
    fn drop(&mut self) {
        self.stop();
    }
}

fn command(
    settings: &Settings,
    entry: &Registration,
    directory: &std::path::Path,
) -> Vec<OsString> {
    let host = entry.route().hostname;
    vec![
        settings.binary.clone().into(),
        "tunnel".into(),
        "--config".into(),
        directory.join("config.yml").into_os_string(),
        "--no-autoupdate".into(),
        "--no-tls-verify=false".into(),
        "--protocol".into(),
        settings.protocol.clone().into(),
        "--pidfile".into(),
        directory.join("connector.pid").into_os_string(),
        "--metrics".into(),
        format!("127.0.0.1:{}", entry.metrics_port).into(),
        "--url".into(),
        "https://127.0.0.1:443".into(),
        "--http-host-header".into(),
        host.clone().into(),
        "--origin-server-name".into(),
        host.into(),
        "--origin-ca-pool".into(),
        directory.join("caddy-root.pem").into_os_string(),
    ]
}
fn local_agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .proxy(None)
        .max_redirects(0)
        .timeout_global(Some(Duration::from_secs(1)))
        .http_status_as_error(false)
        .build()
        .into()
}
fn ready(agent: &ureq::Agent, port: u16) -> bool {
    agent
        .get(format!("http://127.0.0.1:{port}/ready"))
        .call()
        .is_ok_and(|r| r.status() == 200)
}
fn discover(agent: &ureq::Agent, port: u16) -> Result<Option<String>, RunError> {
    let Ok(mut response) = agent
        .get(format!("http://127.0.0.1:{port}/quicktunnel"))
        .call()
    else {
        return Ok(None);
    };
    if response.status() != 200 {
        return Ok(None);
    }
    let value: serde_json::Value = response
        .body_mut()
        .with_config()
        .limit(4096)
        .read_json()
        .map_err(wrapped)?;
    let hostname = value
        .get("hostname")
        .and_then(|v| v.as_str())
        .unwrap_or_default();
    if hostname.is_empty() {
        return Ok(None);
    }
    validate_hostname(hostname).map(Some)
}
fn validate_hostname(host: &str) -> Result<String, RunError> {
    let label = host.strip_suffix(".trycloudflare.com").unwrap_or_default();
    if label.is_empty()
        || label.len() > 63
        || label.starts_with('-')
        || label.ends_with('-')
        || !label
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
    {
        return Err(failure(
            "cloudflared returned an invalid Quick Tunnel hostname",
        ));
    }
    Ok(format!("https://{host}"))
}

#[derive(Debug)]
struct LoopbackOrigin;
impl ureq::unversioned::resolver::Resolver for LoopbackOrigin {
    fn resolve(
        &self,
        _: &ureq::http::Uri,
        _: &ureq::config::Config,
        _: ureq::unversioned::transport::NextTimeout,
    ) -> Result<ureq::unversioned::resolver::ResolvedSocketAddrs, ureq::Error> {
        let mut addresses = self.empty();
        addresses.push(SocketAddr::from((Ipv4Addr::LOCALHOST, 443)));
        Ok(addresses)
    }
}
fn origin_agent(pem: &str) -> Result<ureq::Agent, ureq::Error> {
    let cert = ureq::tls::Certificate::from_pem(pem.as_bytes())?;
    let tls =
        ureq::tls::TlsConfig::builder().root_certs(ureq::tls::RootCerts::new_with_certs(&[cert]));
    #[cfg(windows)]
    let tls = tls.provider(ureq::tls::TlsProvider::NativeTls);
    let config = ureq::Agent::config_builder()
        .proxy(None)
        .max_redirects(0)
        .http_status_as_error(false)
        .timeout_global(Some(Duration::from_secs(1)))
        .tls_config(tls.build())
        .build();
    Ok(ureq::Agent::with_parts(
        config,
        ureq::unversioned::transport::DefaultConnector::default(),
        LoopbackOrigin,
    ))
}

/// Revoke orphaned processes even when Caddy's Admin API is unavailable.
/// Retain the registration until route and filesystem cleanup have succeeded.
pub(crate) fn revoke_orphans(store: &Store) -> Result<Vec<String>, crate::state::Error> {
    store.mutate(|registry| {
        let mut warnings = Vec::new();
        for entry in registry.tunnels.values_mut() {
            if entry.phase == Phase::Stopping
                || process::workspace_supervisor_liveness(&entry.supervisor) == Liveness::Dead
                || (entry.committed && !registry.leases.contains_key(&entry.owner_id))
            {
                entry.phase = Phase::Stopping;
                if let Some(child) = &entry.child
                    && let Err(error) = process::kill_owned_lease(child)
                {
                    warnings.push(error.to_string());
                }
            }
        }
        Ok(warnings)
    })
}

/// Called under the existing operations/state lock; never waits for a process.
/// A surviving/uncertain process keeps its journal, so a later prune can retry.
pub(crate) fn reconcile(
    registry: &mut Registry,
    store: &Store,
    routes: &mut impl RouteBackend,
) -> Vec<String> {
    let mut warnings = Vec::new();
    let mut removed = Vec::new();
    for entry in registry.tunnels.values_mut() {
        let supervisor = process::workspace_supervisor_liveness(&entry.supervisor);
        let owned = !entry.committed || registry.leases.contains_key(&entry.owner_id);
        if entry.phase == Phase::Stopping || supervisor == Liveness::Dead || !owned {
            entry.phase = Phase::Stopping;
            if let Some(child) = &entry.child {
                if let Err(error) = process::kill_owned_lease(child) {
                    warnings.push(error.to_string());
                    continue;
                }
                if process::lease_liveness(child) != Liveness::Dead {
                    continue;
                }
            }
            match routes.remove_if_owned(&entry.route().hostname, entry.id, true) {
                Ok(()) => {
                    let directory = entry.directory(store);
                    match fs::remove_dir_all(directory) {
                        Ok(()) => removed.push(entry.id),
                        Err(e) if e.kind() == io::ErrorKind::NotFound => removed.push(entry.id),
                        Err(e) => warnings.push(e.to_string()),
                    }
                }
                Err(error) => warnings.push(error.to_string()),
            }
        } else if supervisor == Liveness::Alive {
            if let Err(error) = routes.ensure(&entry.route()) {
                warnings.push(error.to_string());
            }
        } else {
            warnings.push(format!(
                "tunnel {} supervisor identity is indeterminate",
                entry.id
            ));
        }
    }
    for id in removed {
        registry.tunnels.remove(&id);
    }
    warnings
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn public_url_has_exact_provider_boundary() {
        assert_eq!(
            validate_hostname("one-two.trycloudflare.com").unwrap(),
            "https://one-two.trycloudflare.com"
        );
        for host in [
            "trycloudflare.com",
            "x.trycloudflare.com.evil",
            "https://x.trycloudflare.com",
            "x.y.trycloudflare.com",
            "-x.trycloudflare.com",
            "x.trycloudflare.com:443",
            "x@a.trycloudflare.com",
            "x\n.trycloudflare.com",
        ] {
            assert!(validate_hostname(host).is_err(), "{host}");
        }
    }
    #[test]
    fn settings_reject_unsafe_or_unbounded_options() {
        assert!(Settings::default().validate().is_ok());
        for protocol in ["", "shell", "http"] {
            assert!(
                Settings {
                    protocol: protocol.into(),
                    ..Settings::default()
                }
                .validate()
                .is_err()
            );
        }
        for seconds in [0, 601, u64::MAX] {
            assert!(
                Settings {
                    startup_timeout_seconds: seconds,
                    ..Settings::default()
                }
                .validate()
                .is_err()
            );
        }
    }
}
