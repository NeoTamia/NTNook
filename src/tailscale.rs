//! Private Tailscale Serve exposure for runs and aliases.
//!
//! The module owns every interaction with the `tailscale` CLI. Nook only runs
//! `tailscale version`, `tailscale status --json`, `tailscale serve status
//! --json`, `tailscale serve --bg --https=<port> <target>`, and `tailscale serve
//! --https=<port> off`. It never runs `serve reset`, `funnel`, `up`, `login`,
//! `set`, or `cert`, never changes ACLs or tailnet settings, and never elevates:
//! the tailnet policy remains the only authorization source.
//!
//! Each registration occupies the root of one tailnet HTTPS port and proxies
//! straight to a loopback upstream, so Tailscale identity headers reach the
//! application exactly as tailscaled sets them.

use std::collections::BTreeSet;
use std::env;
use std::ffi::OsString;
use std::fmt;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use serde::Deserialize;
use serde_json::Value;
use url::Url;
use uuid::Uuid;

use crate::process::{Liveness, RunError};
use crate::state::{Lease, OperationGuard, Registry, ServeRegistration, Store};

pub(crate) const URL_VARIABLE: &str = "NOOK_TAILSCALE_URL";
const PROGRAM_VARIABLE: &str = "NOOK_TAILSCALE";
const MINIMUM_VERSION: (u64, u64) = (1, 52);
const FIRST_PORT: u16 = 443;
const FALLBACK_PORTS: std::ops::RangeInclusive<u16> = 8443..=9442;
const COMMAND_TIMEOUT: Duration = Duration::from_secs(20);
const HTTPS_SETTINGS_URL: &str = "https://login.tailscale.com/admin/dns";
const DOWNLOAD_URL: &str = "https://tailscale.com/download";

#[derive(Debug)]
pub(crate) enum Error {
    MissingClient(PathBuf),
    Launch {
        program: PathBuf,
        source: io::Error,
    },
    Timeout(String),
    UnsupportedVersion(String),
    DaemonUnavailable(String),
    NotRunning {
        state: String,
        auth_url: Option<String>,
    },
    HttpsDisabled,
    MissingDnsName,
    AccessDenied(String),
    Command {
        command: String,
        message: String,
    },
    InvalidOutput {
        command: String,
        reason: String,
    },
    NonLoopbackTarget(String),
    NoFreePort,
    Unconfirmed(u16),
    FunnelEnabled(u16),
    State(crate::state::Error),
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingClient(program) => write!(
                formatter,
                "Tailscale CLI `{}` was not found; install Tailscale from {DOWNLOAD_URL} or set {PROGRAM_VARIABLE} to its path (Nook never installs or starts Tailscale)",
                program.display()
            ),
            Self::Launch { program, source } => write!(
                formatter,
                "cannot run Tailscale CLI `{}`: {source}",
                program.display()
            ),
            Self::Timeout(command) => write!(
                formatter,
                "`{command}` did not finish within {} seconds; check that tailscaled is responsive",
                COMMAND_TIMEOUT.as_secs()
            ),
            Self::UnsupportedVersion(version) => write!(
                formatter,
                "Tailscale {version} does not support background Serve; update Tailscale to {}.{} or newer",
                MINIMUM_VERSION.0, MINIMUM_VERSION.1
            ),
            Self::DaemonUnavailable(message) => write!(
                formatter,
                "tailscaled is not reachable ({message}); start the Tailscale service yourself, Nook never starts it"
            ),
            Self::NotRunning {
                state,
                auth_url: Some(url),
            } => write!(
                formatter,
                "Tailscale is not connected (state {state}); authenticate this device at {url}"
            ),
            Self::NotRunning {
                state,
                auth_url: None,
            } => match state.as_str() {
                "NeedsMachineAuth" => write!(
                    formatter,
                    "Tailscale is waiting for a tailnet admin to approve this device (state {state})"
                ),
                "Starting" | "NoState" => write!(
                    formatter,
                    "Tailscale is still starting (state {state}); retry once it is connected"
                ),
                _ => write!(
                    formatter,
                    "Tailscale is not connected (state {state}); run `tailscale up` yourself, Nook never connects it"
                ),
            },
            Self::HttpsDisabled => write!(
                formatter,
                "HTTPS certificates are not enabled for this tailnet, so Tailscale Serve cannot publish HTTPS URLs; a tailnet admin can enable them at {HTTPS_SETTINGS_URL} (Nook never changes tailnet settings)"
            ),
            Self::MissingDnsName => write!(
                formatter,
                "Tailscale reports no MagicDNS name for this device; enable MagicDNS at {HTTPS_SETTINGS_URL}"
            ),
            Self::AccessDenied(message) => write!(
                formatter,
                "Tailscale refused to change Serve for this user ({message}); Nook never elevates, so allow your user explicitly, for example with `sudo tailscale set --operator=$USER`"
            ),
            Self::Command { command, message } => {
                write!(formatter, "`{command}` failed: {message}")
            }
            Self::InvalidOutput { command, reason } => {
                write!(
                    formatter,
                    "`{command}` returned unexpected output: {reason}"
                )
            }
            Self::NonLoopbackTarget(target) => write!(
                formatter,
                "Tailscale Serve exposure requires a loopback upstream (127.0.0.1 or localhost); `{target}` is not one"
            ),
            Self::NoFreePort => write!(
                formatter,
                "no free Tailscale Serve HTTPS port is left among {FIRST_PORT} and {}-{}",
                FALLBACK_PORTS.start(),
                FALLBACK_PORTS.end()
            ),
            Self::Unconfirmed(port) => write!(
                formatter,
                "Tailscale did not report the Serve registration on port {port} after creating it"
            ),
            Self::FunnelEnabled(port) => write!(
                formatter,
                "port {port} is exposed through Tailscale Funnel; Nook only manages private Serve and removed its registration"
            ),
            Self::State(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Launch { source, .. } => Some(source),
            Self::State(error) => Some(error),
            _ => None,
        }
    }
}

impl From<crate::state::Error> for Error {
    fn from(error: crate::state::Error) -> Self {
        Self::State(error)
    }
}

/// Validates that `target` is a loopback upstream Tailscale Serve can proxy
/// to and returns it in the form passed to `tailscale serve`.
pub(crate) fn serve_target(target: &str) -> Result<String, Error> {
    let rejected = || Error::NonLoopbackTarget(target.to_owned());
    let url = Url::parse(target).map_err(|_| rejected())?;
    let host = url.host_str().ok_or_else(rejected)?;
    if !matches!(url.scheme(), "http" | "https")
        || !matches!(host, "127.0.0.1" | "localhost")
        || !matches!(url.path(), "" | "/")
        || url.query().is_some()
        || url.fragment().is_some()
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return Err(rejected());
    }
    let port = url.port_or_known_default().ok_or_else(rejected)?;
    Ok(format!("{}://{host}:{port}", url.scheme()))
}

fn same_target(left: &str, right: &str) -> bool {
    matches!((serve_target(left), serve_target(right)), (Ok(left), Ok(right)) if left == right)
}

pub(crate) fn serve_url(dns_name: &str, port: u16) -> String {
    if port == FIRST_PORT {
        format!("https://{dns_name}")
    } else {
        format!("https://{dns_name}:{port}")
    }
}

/// Returns the tailnet URL recorded for a lease or alias, if it has one.
pub(crate) fn recorded_url(registry: &Registry, owner_id: Uuid) -> Option<String> {
    let dns_name = registry.tailscale.dns_name.as_deref()?;
    registry
        .tailscale
        .registrations
        .iter()
        .find(|(_, registration)| registration.owner_id == owner_id)
        .map(|(port, _)| serve_url(dns_name, *port))
}

fn candidate_ports() -> impl Iterator<Item = u16> {
    std::iter::once(FIRST_PORT).chain(FALLBACK_PORTS)
}

fn allocate_port(preferred: Option<u16>, taken: &BTreeSet<u16>) -> Option<u16> {
    preferred
        .filter(|port| !taken.contains(port))
        .or_else(|| candidate_ports().find(|port| !taken.contains(port)))
}

fn parse_version(output: &str) -> Option<(u64, u64)> {
    let first = output.lines().next()?.trim();
    let mut parts = first.split(['.', '-']);
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next()?.parse().ok()?;
    Some((major, minor))
}

#[derive(Debug, Deserialize)]
struct StatusJson {
    #[serde(rename = "BackendState", default)]
    backend_state: String,
    #[serde(rename = "AuthURL", default)]
    auth_url: String,
    #[serde(rename = "Self")]
    this_device: Option<DeviceJson>,
    #[serde(rename = "CertDomains", default)]
    cert_domains: Option<Vec<String>>,
}

#[derive(Debug, Deserialize)]
struct DeviceJson {
    #[serde(rename = "DNSName", default)]
    dns_name: String,
}

/// Diagnosis of the local Tailscale installation, filled as far as the checks
/// got. `result` is the verdict used to refuse a change.
#[derive(Debug)]
pub(crate) struct Diagnosis {
    pub(crate) version: Option<String>,
    pub(crate) backend_state: Option<String>,
    pub(crate) https_enabled: Option<bool>,
    pub(crate) dns_name: Option<String>,
    pub(crate) result: Result<(), Error>,
}

impl Diagnosis {
    fn ready_dns_name(self) -> Result<String, Error> {
        self.result?;
        self.dns_name.ok_or(Error::MissingDnsName)
    }
}

/// Parsed `tailscale serve status --json`.
#[derive(Debug, Clone, Default)]
struct ServeConfig(Value);

impl ServeConfig {
    fn occupied_ports(&self) -> BTreeSet<u16> {
        let mut ports = BTreeSet::new();
        collect_ports(&self.0, &mut ports);
        ports
    }

    fn owns(&self, dns_name: &str, port: u16, target: &str) -> bool {
        let https = self
            .0
            .pointer(&format!("/TCP/{port}/HTTPS"))
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let proxy = self
            .0
            .get("Web")
            .and_then(|web| web.get(format!("{dns_name}:{port}")))
            .and_then(|host| host.pointer("/Handlers/~1/Proxy"))
            .and_then(Value::as_str);
        https && proxy.is_some_and(|proxy| same_target(proxy, target))
    }

    fn funnel_enabled(&self, port: u16) -> bool {
        self.0
            .get("AllowFunnel")
            .and_then(Value::as_object)
            .is_some_and(|hosts| {
                hosts.iter().any(|(host, enabled)| {
                    enabled.as_bool() == Some(true) && host_port(host) == Some(port)
                })
            })
    }

    fn observe(&self, dns_name: &str, port: u16, target: &str) -> Observation {
        if self.owns(dns_name, port, target) {
            Observation::Owned
        } else if self.occupied_ports().contains(&port) {
            Observation::Foreign
        } else {
            Observation::Absent
        }
    }
}

fn collect_ports(config: &Value, ports: &mut BTreeSet<u16>) {
    if let Some(tcp) = config.get("TCP").and_then(Value::as_object) {
        ports.extend(tcp.keys().filter_map(|port| port.parse::<u16>().ok()));
    }
    if let Some(web) = config.get("Web").and_then(Value::as_object) {
        ports.extend(web.keys().filter_map(|host| host_port(host)));
    }
    if let Some(sessions) = config.get("Foreground").and_then(Value::as_object) {
        for session in sessions.values() {
            collect_ports(session, ports);
        }
    }
}

fn host_port(host: &str) -> Option<u16> {
    host.rsplit_once(':')?.1.parse().ok()
}

/// What Tailscale currently serves on a port Nook has recorded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Observation {
    Owned,
    Absent,
    Foreign,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Owner {
    Alive,
    Gone,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Action {
    Keep,
    Restore,
    Conflict,
    Remove,
    Forget,
    ForgetForeign,
}

fn plan(owner: Owner, observation: Observation) -> Action {
    match (owner, observation) {
        (Owner::Unknown, _) | (Owner::Alive, Observation::Owned) => Action::Keep,
        (Owner::Alive, Observation::Absent) => Action::Restore,
        (Owner::Alive, Observation::Foreign) => Action::Conflict,
        (Owner::Gone, Observation::Owned) => Action::Remove,
        (Owner::Gone, Observation::Absent) => Action::Forget,
        (Owner::Gone, Observation::Foreign) => Action::ForgetForeign,
    }
}

fn owner_state(
    registry: &Registry,
    owner_id: Uuid,
    liveness: &mut impl FnMut(&Lease) -> Liveness,
) -> Owner {
    if registry.aliases.values().any(|alias| alias.id == owner_id) {
        return Owner::Alive;
    }
    match registry.leases.get(&owner_id).map(liveness) {
        Some(Liveness::Alive) => Owner::Alive,
        Some(Liveness::Indeterminate) => Owner::Unknown,
        Some(Liveness::Dead) | None => Owner::Gone,
    }
}

#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Report {
    pub(crate) restored: usize,
    pub(crate) removed: usize,
    pub(crate) warnings: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Registration {
    pub(crate) port: u16,
    pub(crate) url: String,
    pub(crate) warnings: Vec<String>,
}

/// One recorded registration as observed by `nook tailscale status`.
#[derive(Debug)]
pub(crate) struct ObservedRegistration {
    pub(crate) port: u16,
    pub(crate) registration: ServeRegistration,
    pub(crate) url: Option<String>,
    pub(crate) observation: Option<Observation>,
}

struct Output {
    success: bool,
    stdout: String,
    stderr: String,
}

/// Runs the Tailscale CLI with a bounded duration.
#[derive(Debug, Clone)]
pub(crate) struct Client {
    program: PathBuf,
    overridden: bool,
}

impl Client {
    /// Uses `NOOK_TAILSCALE` when set, otherwise `tailscale` (`tailscale.exe`
    /// on Windows) from `PATH`.
    pub(crate) fn from_environment() -> Self {
        match env::var_os(PROGRAM_VARIABLE).filter(|value| !value.is_empty()) {
            Some(program) => Self {
                program: PathBuf::from(program),
                overridden: true,
            },
            None => Self {
                program: PathBuf::from("tailscale"),
                overridden: false,
            },
        }
    }

    fn run(&self, arguments: &[&str]) -> Result<Output, Error> {
        match run_with_timeout(&self.program, arguments) {
            Err(Error::MissingClient(_)) if !self.overridden => match windows_install_location() {
                Some(program) => run_with_timeout(&program, arguments),
                None => Err(Error::MissingClient(self.program.clone())),
            },
            result => result,
        }
    }

    fn checked(&self, arguments: &[&str]) -> Result<String, Error> {
        let output = self.run(arguments)?;
        if output.success {
            return Ok(output.stdout);
        }
        let message = first_line(&output.stderr)
            .or_else(|| first_line(&output.stdout))
            .unwrap_or("exited unsuccessfully")
            .to_owned();
        let lowered = message.to_ascii_lowercase();
        if lowered.contains("failed to connect to local tailscale")
            || lowered.contains("doesn't appear to be running")
            || lowered.contains("is tailscale running")
        {
            return Err(Error::DaemonUnavailable(message));
        }
        if lowered.contains("access denied") || lowered.contains("permission denied") {
            return Err(Error::AccessDenied(message));
        }
        Err(Error::Command {
            command: command_line(arguments),
            message,
        })
    }

    /// Checks client, daemon, login, and HTTPS enablement without changing
    /// anything.
    pub(crate) fn diagnose(&self) -> Diagnosis {
        let mut diagnosis = Diagnosis {
            version: None,
            backend_state: None,
            https_enabled: None,
            dns_name: None,
            result: Ok(()),
        };
        diagnosis.result = self.diagnose_into(&mut diagnosis);
        diagnosis
    }

    fn diagnose_into(&self, diagnosis: &mut Diagnosis) -> Result<(), Error> {
        let version = self.checked(&["version"])?;
        let label = first_line(&version).unwrap_or_default().to_owned();
        diagnosis.version = Some(label.clone());
        match parse_version(&version) {
            Some(version) if version >= MINIMUM_VERSION => {}
            _ => return Err(Error::UnsupportedVersion(label)),
        }
        let command = ["status", "--json"];
        let status: StatusJson =
            serde_json::from_str(&self.checked(&command)?).map_err(|error| {
                Error::InvalidOutput {
                    command: command_line(&command),
                    reason: error.to_string(),
                }
            })?;
        diagnosis.backend_state = Some(status.backend_state.clone());
        if let Some(dns_name) = status
            .this_device
            .map(|device| device.dns_name.trim_end_matches('.').to_owned())
            .filter(|name| !name.is_empty())
        {
            diagnosis.dns_name = Some(dns_name);
        }
        if status.backend_state != "Running" {
            return Err(Error::NotRunning {
                state: status.backend_state,
                auth_url: Some(status.auth_url).filter(|url| !url.is_empty()),
            });
        }
        let https_enabled = status
            .cert_domains
            .is_some_and(|domains| !domains.is_empty());
        diagnosis.https_enabled = Some(https_enabled);
        if !https_enabled {
            return Err(Error::HttpsDisabled);
        }
        if diagnosis.dns_name.is_none() {
            return Err(Error::MissingDnsName);
        }
        Ok(())
    }

    /// Fails with an actionable diagnosis unless Serve can be used now.
    pub(crate) fn require_ready(&self) -> Result<(), Error> {
        self.diagnose().ready_dns_name().map(|_| ())
    }

    fn serve_config(&self) -> Result<ServeConfig, Error> {
        let command = ["serve", "status", "--json"];
        let stdout = self.checked(&command)?;
        if stdout.trim().is_empty() {
            return Ok(ServeConfig::default());
        }
        let value: Value = serde_json::from_str(&stdout).map_err(|error| Error::InvalidOutput {
            command: command_line(&command),
            reason: error.to_string(),
        })?;
        match value {
            Value::Null => Ok(ServeConfig::default()),
            Value::Object(_) => Ok(ServeConfig(value)),
            _ => Err(Error::InvalidOutput {
                command: command_line(&command),
                reason: "expected a JSON object".into(),
            }),
        }
    }

    fn serve_on(&self, port: u16, target: &str) -> Result<(), Error> {
        self.checked(&["serve", "--bg", &format!("--https={port}"), target])
            .map(|_| ())
    }

    fn serve_off(&self, port: u16) -> Result<(), Error> {
        self.checked(&["serve", &format!("--https={port}"), "off"])
            .map(|_| ())
    }

    /// Allocates a tailnet HTTPS port and publishes `target` on it.
    ///
    /// The registration is journaled before Serve is changed, so an
    /// interruption is converged by the next reconciliation.
    pub(crate) fn register(
        &self,
        operations: &OperationGuard,
        store: &Store,
        owner_id: Uuid,
        hostname: &str,
        target: &str,
        mut liveness: impl FnMut(&Lease) -> Liveness,
    ) -> Result<Registration, Error> {
        let target = serve_target(target)?;
        let dns_name = self.diagnose().ready_dns_name()?;
        let mut config = self.serve_config()?;
        let report = self.converge(operations, store, &dns_name, &config, &mut liveness)?;
        if report.removed > 0 {
            config = self.serve_config()?;
        }
        let occupied = config.occupied_ports();
        let port = store.mutate(|registry| {
            let taken: BTreeSet<u16> = occupied
                .iter()
                .chain(registry.tailscale.registrations.keys())
                .copied()
                .collect();
            let preferred = registry.tailscale.preferred_ports.get(hostname).copied();
            let Some(port) = allocate_port(preferred, &taken) else {
                return Ok(None);
            };
            registry.tailscale.registrations.insert(
                port,
                ServeRegistration {
                    owner_id,
                    hostname: hostname.to_owned(),
                    target: target.clone(),
                },
            );
            registry
                .tailscale
                .preferred_ports
                .insert(hostname.to_owned(), port);
            registry.tailscale.dns_name = Some(dns_name.clone());
            Ok(Some(port))
        })?;
        let port = port.ok_or(Error::NoFreePort)?;
        let applied = self.serve_on(port, &target).and_then(|()| {
            let config = self.serve_config()?;
            if config.funnel_enabled(port) {
                Err(Error::FunnelEnabled(port))
            } else if config.owns(&dns_name, port, &target) {
                Ok(())
            } else {
                Err(Error::Unconfirmed(port))
            }
        });
        if let Err(error) = applied {
            let _ = self.release(store, &dns_name, port, owner_id);
            return Err(error);
        }
        Ok(Registration {
            port,
            url: serve_url(&dns_name, port),
            warnings: report.warnings,
        })
    }

    /// Removes the registrations of `owner_id` that Nook still owns.
    pub(crate) fn withdraw(
        &self,
        _operations: &OperationGuard,
        store: &Store,
        owner_id: Uuid,
    ) -> Vec<String> {
        let registry = match store.load() {
            Ok(registry) => registry,
            Err(error) => return vec![error.to_string()],
        };
        let Some(dns_name) = registry.tailscale.dns_name.clone() else {
            return Vec::new();
        };
        registry
            .tailscale
            .registrations
            .iter()
            .filter(|(_, registration)| registration.owner_id == owner_id)
            .filter_map(|(port, _)| {
                self.release(store, &dns_name, *port, owner_id)
                    .err()
                    .map(|error| {
                        format!("Tailscale Serve cleanup of port {port} is pending: {error}")
                    })
            })
            .collect()
    }

    fn release(
        &self,
        store: &Store,
        dns_name: &str,
        port: u16,
        owner_id: Uuid,
    ) -> Result<(), Error> {
        let registry = store.load()?;
        let Some(registration) = registry
            .tailscale
            .registrations
            .get(&port)
            .filter(|registration| registration.owner_id == owner_id)
        else {
            return Ok(());
        };
        if self
            .serve_config()?
            .owns(dns_name, port, &registration.target)
        {
            self.serve_off(port)?;
        }
        forget(store, &[(port, owner_id)])?;
        Ok(())
    }

    /// Restores registrations of live owners and removes those whose owner is
    /// gone. Tailscale being unavailable only defers the work.
    pub(crate) fn reconcile(
        &self,
        operations: &OperationGuard,
        store: &Store,
        mut liveness: impl FnMut(&Lease) -> Liveness,
    ) -> Result<Report, Error> {
        let pending = store.load()?.tailscale.registrations.len();
        if pending == 0 {
            return Ok(Report::default());
        }
        let deferred = |error: Error| Report {
            warnings: vec![format!(
                "{pending} Tailscale Serve registration(s) are pending: {error}"
            )],
            ..Report::default()
        };
        let dns_name = match self.diagnose().ready_dns_name() {
            Ok(dns_name) => dns_name,
            Err(error) => return Ok(deferred(error)),
        };
        let config = match self.serve_config() {
            Ok(config) => config,
            Err(error) => return Ok(deferred(error)),
        };
        self.converge(operations, store, &dns_name, &config, &mut liveness)
    }

    fn converge(
        &self,
        _operations: &OperationGuard,
        store: &Store,
        dns_name: &str,
        config: &ServeConfig,
        liveness: &mut impl FnMut(&Lease) -> Liveness,
    ) -> Result<Report, Error> {
        let registry = store.load()?;
        let mut report = Report::default();
        let mut forgotten = Vec::new();
        for (port, registration) in &registry.tailscale.registrations {
            let port = *port;
            let owner = owner_state(&registry, registration.owner_id, liveness);
            match plan(owner, config.observe(dns_name, port, &registration.target)) {
                Action::Keep => {}
                Action::Restore => match self.serve_on(port, &registration.target) {
                    Ok(()) => report.restored += 1,
                    Err(error) => report.warnings.push(format!(
                        "restoration of Tailscale Serve port {port} for {} is pending: {error}",
                        registration.hostname
                    )),
                },
                Action::Conflict => report.warnings.push(format!(
                    "Tailscale Serve port {port} of {} is now used by a configuration Nook does not own; Nook left it untouched",
                    registration.hostname
                )),
                Action::Remove => match self.serve_off(port) {
                    Ok(()) => {
                        report.removed += 1;
                        forgotten.push((port, registration.owner_id));
                    }
                    Err(error) => report.warnings.push(format!(
                        "Tailscale Serve cleanup of port {port} for {} is pending: {error}",
                        registration.hostname
                    )),
                },
                Action::Forget => forgotten.push((port, registration.owner_id)),
                Action::ForgetForeign => {
                    forgotten.push((port, registration.owner_id));
                    report.warnings.push(format!(
                        "Tailscale Serve port {port} of {} was replaced by a configuration Nook does not own; Nook left it untouched",
                        registration.hostname
                    ));
                }
            }
        }
        store.mutate(|registry| {
            for (port, owner_id) in &forgotten {
                if registry
                    .tailscale
                    .registrations
                    .get(port)
                    .is_some_and(|registration| registration.owner_id == *owner_id)
                {
                    registry.tailscale.registrations.remove(port);
                }
            }
            registry.tailscale.dns_name = Some(dns_name.to_owned());
            Ok(())
        })?;
        Ok(report)
    }

    /// Observes every recorded registration without changing anything.
    pub(crate) fn observe(
        &self,
        registry: &Registry,
        diagnosis: &Diagnosis,
    ) -> Vec<ObservedRegistration> {
        let ready_dns_name = diagnosis
            .result
            .as_ref()
            .ok()
            .and(diagnosis.dns_name.as_deref());
        let config = ready_dns_name.and_then(|_| self.serve_config().ok());
        let dns_name = ready_dns_name.or(registry.tailscale.dns_name.as_deref());
        registry
            .tailscale
            .registrations
            .iter()
            .map(|(port, registration)| ObservedRegistration {
                port: *port,
                registration: registration.clone(),
                url: dns_name.map(|dns_name| serve_url(dns_name, *port)),
                observation: config
                    .as_ref()
                    .zip(ready_dns_name)
                    .map(|(config, dns_name)| {
                        config.observe(dns_name, *port, &registration.target)
                    }),
            })
            .collect()
    }
}

fn forget(store: &Store, registrations: &[(u16, Uuid)]) -> Result<(), crate::state::Error> {
    store.mutate(|registry| {
        for (port, owner_id) in registrations {
            if registry
                .tailscale
                .registrations
                .get(port)
                .is_some_and(|registration| registration.owner_id == *owner_id)
            {
                registry.tailscale.registrations.remove(port);
            }
        }
        Ok(())
    })
}

/// Publishes a run on Tailscale Serve between its Caddy route and its spawn.
pub(crate) struct ServeExposure<'a> {
    client: &'a Client,
    pub(crate) registration: Option<Registration>,
}

impl<'a> ServeExposure<'a> {
    pub(crate) fn new(client: &'a Client) -> Self {
        Self {
            client,
            registration: None,
        }
    }
}

impl crate::process::RunExposure for ServeExposure<'_> {
    fn expose(
        &mut self,
        operations: &OperationGuard,
        store: &Store,
        owner_id: Uuid,
        hostname: &str,
        target: &str,
    ) -> Result<Vec<(OsString, OsString)>, RunError> {
        let registration = self
            .client
            .register(
                operations,
                store,
                owner_id,
                hostname,
                target,
                crate::process::lease_liveness,
            )
            .map_err(|error| RunError::Exposure(Box::new(error)))?;
        let variable = (
            OsString::from(URL_VARIABLE),
            OsString::from(&registration.url),
        );
        self.registration = Some(registration);
        Ok(vec![variable])
    }

    fn withdraw(&mut self, operations: &OperationGuard, store: &Store, owner_id: Uuid) {
        self.client.withdraw(operations, store, owner_id);
        self.registration = None;
    }
}

fn run_with_timeout(program: &Path, arguments: &[&str]) -> Result<Output, Error> {
    let mut child = Command::new(program)
        .args(arguments)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|source| {
            if source.kind() == io::ErrorKind::NotFound {
                Error::MissingClient(program.to_path_buf())
            } else {
                Error::Launch {
                    program: program.to_path_buf(),
                    source,
                }
            }
        })?;
    let stdout = child.stdout.take().map(read_in_background);
    let stderr = child.stderr.take().map(read_in_background);
    let deadline = Instant::now() + COMMAND_TIMEOUT;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(10)),
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(Error::Timeout(command_line(arguments)));
            }
            Err(source) => {
                return Err(Error::Launch {
                    program: program.to_path_buf(),
                    source,
                });
            }
        }
    };
    let collect = |reader: Option<thread::JoinHandle<String>>| {
        reader
            .and_then(|reader| reader.join().ok())
            .unwrap_or_default()
    };
    Ok(Output {
        success: status.success(),
        stdout: collect(stdout),
        stderr: collect(stderr),
    })
}

fn read_in_background(mut pipe: impl Read + Send + 'static) -> thread::JoinHandle<String> {
    thread::spawn(move || {
        let mut bytes = Vec::new();
        let _ = pipe.read_to_end(&mut bytes);
        String::from_utf8_lossy(&bytes).into_owned()
    })
}

#[cfg(windows)]
fn windows_install_location() -> Option<PathBuf> {
    env::var_os("ProgramFiles")
        .map(|directory| {
            PathBuf::from(directory)
                .join("Tailscale")
                .join("tailscale.exe")
        })
        .filter(|program| program.is_file())
}

#[cfg(not(windows))]
fn windows_install_location() -> Option<PathBuf> {
    None
}

fn first_line(text: &str) -> Option<&str> {
    text.lines().map(str::trim).find(|line| !line.is_empty())
}

fn command_line(arguments: &[&str]) -> String {
    std::iter::once("tailscale")
        .chain(arguments.iter().copied())
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use serde_json::json;
    use uuid::Uuid;

    use super::{
        Action, Observation, Owner, ServeConfig, allocate_port, owner_state, parse_version, plan,
        recorded_url, serve_target, serve_url,
    };
    use crate::process::Liveness;
    use crate::state::{Alias, Lease, LeaseState, Registry, Scheme, ServeRegistration};

    #[test]
    fn first_port_is_443_then_8443_upward_skipping_every_taken_port() {
        assert_eq!(allocate_port(None, &BTreeSet::new()), Some(443));
        assert_eq!(allocate_port(None, &BTreeSet::from([443])), Some(8443));
        assert_eq!(
            allocate_port(None, &BTreeSet::from([443, 8443, 8445])),
            Some(8444)
        );
        let exhausted: BTreeSet<u16> = std::iter::once(443).chain(8443..=9442).collect();
        assert_eq!(allocate_port(None, &exhausted), None);
    }

    #[test]
    fn a_hostname_gets_its_previous_port_back_when_it_is_still_free() {
        assert_eq!(
            allocate_port(Some(8444), &BTreeSet::from([443])),
            Some(8444)
        );
        assert_eq!(
            allocate_port(Some(8444), &BTreeSet::from([8444])),
            Some(443)
        );
    }

    #[test]
    fn only_loopback_upstreams_are_accepted_and_normalized_for_serve() {
        assert_eq!(
            serve_target("http://127.0.0.1:3000/").unwrap(),
            "http://127.0.0.1:3000"
        );
        assert_eq!(
            serve_target("https://localhost/").unwrap(),
            "https://localhost:443"
        );
        for rejected in [
            "https://service.internal:8443/",
            "http://192.168.1.10:3000/",
            "http://0.0.0.0:3000/",
            "http://[::1]:3000/",
            "http://127.0.0.1:3000/admin",
            "ftp://127.0.0.1:21/",
            "not a url",
        ] {
            assert!(serve_target(rejected).is_err(), "{rejected} was accepted");
        }
    }

    #[test]
    fn tailnet_urls_omit_the_default_https_port() {
        assert_eq!(serve_url("host.ts.net", 443), "https://host.ts.net");
        assert_eq!(serve_url("host.ts.net", 8443), "https://host.ts.net:8443");
    }

    #[test]
    fn versions_before_background_serve_are_detected() {
        assert_eq!(
            parse_version("1.76.1\n  tailscale commit: abc"),
            Some((1, 76))
        );
        assert_eq!(parse_version("1.51.0-t1234"), Some((1, 51)));
        assert!(parse_version("1.51.0").unwrap() < super::MINIMUM_VERSION);
        assert_eq!(parse_version("garbage"), None);
    }

    #[test]
    fn every_port_used_by_any_serve_configuration_is_occupied() {
        let config = ServeConfig(json!({
            "TCP": {"443": {"HTTPS": true}, "5432": {"TCPForward": "127.0.0.1:5432"}},
            "Web": {"host.ts.net:8443": {"Handlers": {"/": {"Proxy": "http://127.0.0.1:1"}}}},
            "Foreground": {"session": {"TCP": {"8444": {"HTTPS": true}}}},
            "Services": {"svc:db": {"TCP": {"9000": {"HTTPS": true}}}}
        }));
        assert_eq!(
            config.occupied_ports(),
            BTreeSet::from([443, 5432, 8443, 8444])
        );
    }

    #[test]
    fn ownership_requires_the_recorded_upstream_on_the_root_of_the_port() {
        let config = ServeConfig(json!({
            "TCP": {"443": {"HTTPS": true}, "8443": {"HTTPS": true}},
            "Web": {
                "host.ts.net:443": {"Handlers": {"/": {"Proxy": "http://127.0.0.1:3000"}}},
                "host.ts.net:8443": {"Handlers": {"/docs": {"Proxy": "http://127.0.0.1:4000"}}}
            }
        }));
        assert_eq!(
            config.observe("host.ts.net", 443, "http://127.0.0.1:3000/"),
            Observation::Owned
        );
        assert_eq!(
            config.observe("host.ts.net", 443, "http://127.0.0.1:3001"),
            Observation::Foreign
        );
        assert_eq!(
            config.observe("host.ts.net", 8443, "http://127.0.0.1:4000"),
            Observation::Foreign
        );
        assert_eq!(
            config.observe("renamed.ts.net", 443, "http://127.0.0.1:3000"),
            Observation::Foreign
        );
        assert_eq!(
            config.observe("host.ts.net", 8444, "http://127.0.0.1:3000"),
            Observation::Absent
        );
    }

    #[test]
    fn funnel_is_detected_per_port() {
        let config = ServeConfig(
            json!({"AllowFunnel": {"host.ts.net:443": true, "host.ts.net:8443": false}}),
        );
        assert!(config.funnel_enabled(443));
        assert!(!config.funnel_enabled(8443));
    }

    #[test]
    fn foreign_configuration_is_never_removed_or_overwritten() {
        for owner in [Owner::Alive, Owner::Gone, Owner::Unknown] {
            let action = plan(owner, Observation::Foreign);
            assert!(
                !matches!(action, Action::Remove | Action::Restore),
                "{owner:?} on a foreign port planned {action:?}"
            );
        }
    }

    #[test]
    fn live_owners_are_restored_and_gone_owners_are_cleaned() {
        assert_eq!(plan(Owner::Alive, Observation::Absent), Action::Restore);
        assert_eq!(plan(Owner::Alive, Observation::Owned), Action::Keep);
        assert_eq!(plan(Owner::Gone, Observation::Owned), Action::Remove);
        assert_eq!(plan(Owner::Gone, Observation::Absent), Action::Forget);
        assert_eq!(plan(Owner::Unknown, Observation::Absent), Action::Keep);
        assert_eq!(plan(Owner::Unknown, Observation::Owned), Action::Keep);
    }

    #[test]
    fn aliases_and_live_leases_own_registrations_but_dead_leases_do_not() {
        let mut registry = Registry::empty();
        let alias = Alias {
            id: Uuid::new_v4(),
            hostname: "alias.localhost".into(),
            target: "http://127.0.0.1:3000/".into(),
            scheme: Scheme::Http,
            tls: true,
            preserve_host: false,
        };
        let lease = Lease {
            id: Uuid::new_v4(),
            hostname: "run.localhost".into(),
            target: "http://127.0.0.1:3001".into(),
            scheme: Scheme::Http,
            tls: true,
            pid: 1,
            pgid: 1,
            process_start_time_ticks: 1,
            state: LeaseState::Ready,
        };
        registry
            .aliases
            .insert(alias.hostname.clone(), alias.clone());
        registry.leases.insert(lease.id, lease.clone());
        let mut dead = |_: &Lease| Liveness::Dead;
        let mut unknown = |_: &Lease| Liveness::Indeterminate;
        assert_eq!(owner_state(&registry, alias.id, &mut dead), Owner::Alive);
        assert_eq!(owner_state(&registry, lease.id, &mut dead), Owner::Gone);
        assert_eq!(
            owner_state(&registry, lease.id, &mut unknown),
            Owner::Unknown
        );
        assert_eq!(
            owner_state(&registry, Uuid::new_v4(), &mut unknown),
            Owner::Gone
        );
    }

    #[test]
    fn recorded_url_uses_the_last_observed_device_name() {
        let mut registry = Registry::empty();
        let owner = Uuid::new_v4();
        registry.tailscale.registrations.insert(
            8443,
            ServeRegistration {
                owner_id: owner,
                hostname: "api.localhost".into(),
                target: "http://127.0.0.1:3000".into(),
            },
        );
        assert_eq!(recorded_url(&registry, owner), None);
        registry.tailscale.dns_name = Some("host.ts.net".into());
        assert_eq!(
            recorded_url(&registry, owner).as_deref(),
            Some("https://host.ts.net:8443")
        );
        assert_eq!(recorded_url(&registry, Uuid::new_v4()), None);
    }
}
