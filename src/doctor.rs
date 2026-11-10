//! Inspection is deliberately separate from reconciliation and its recovery guards.
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::io::{self, Write};
use std::net::IpAddr;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use serde::Serialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::{caddy, config, doctor_io, process, reconcile, state};

#[derive(Debug, clap::Args)]
pub(crate) struct Args {
    /// Emit one versioned JSON report.
    #[arg(long)]
    json: bool,
    /// Send DNS, TCP, TLS, HTTP HEAD and mDNS probes (services may log/act on traffic).
    #[arg(long)]
    deep: bool,
    /// Total observation budget, 1s..300s (default: 10s quick, 30s deep).
    #[arg(long, value_parser = parse_timeout)]
    timeout: Option<Duration>,
    /// Inspect one recorded owner instead of simulating project configuration.
    #[arg(long, conflicts_with_all = ["config", "local", "name", "worktree", "no_worktree", "only", "runner", "command", "no_tls", "lan", "no_lan", "tailscale", "funnel", "no_tailscale", "run_bind_address", "app_port", "strict_port", "readiness_warn_after", "framework", "no_framework"])]
    owner: Option<Uuid>,
    /// Inspect an existing public CA PEM; never fetch or install a CA.
    #[arg(long)]
    ca_cert: Option<PathBuf>,
    #[arg(long)]
    config: Option<PathBuf>,
    #[arg(long, requires = "config")]
    local: bool,
    #[arg(long)]
    name: Option<String>,
    #[arg(long)]
    no_tls: bool,
    #[arg(long, conflicts_with = "no_lan")]
    lan: bool,
    #[arg(long)]
    no_lan: bool,
    #[arg(long)]
    tailscale: bool,
    #[arg(long, conflicts_with = "tailscale")]
    funnel: bool,
    #[arg(long, conflicts_with_all = ["tailscale", "funnel"])]
    no_tailscale: bool,
    #[arg(long, num_args = 0..=1, require_equals = true, default_missing_value = "", conflicts_with = "no_worktree")]
    worktree: Option<String>,
    #[arg(long)]
    no_worktree: bool,
    #[arg(long)]
    only: Option<String>,
    #[arg(long, conflicts_with = "only")]
    runner: bool,
    #[arg(long)]
    run_bind_address: Option<IpAddr>,
    #[arg(long)]
    app_port: Option<u16>,
    #[arg(long, requires = "app_port")]
    strict_port: bool,
    #[arg(long)]
    readiness_warn_after: Option<u64>,
    #[arg(long, value_parser = ["vite", "nuxt", "astro", "none"])]
    framework: Option<String>,
    #[arg(long, conflicts_with = "framework")]
    no_framework: bool,
    /// Hypothetical child argv; never executed.
    #[arg(last = true)]
    command: Vec<OsString>,
}

fn parse_timeout(value: &str) -> Result<Duration, String> {
    let seconds = value
        .strip_suffix('s')
        .unwrap_or(value)
        .parse::<u64>()
        .map_err(|_| "expected 1s..300s")?;
    if !(1..=300).contains(&seconds) {
        return Err("expected 1s..300s".into());
    }
    Ok(Duration::from_secs(seconds))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Status {
    Pass,
    Fail,
    Warning,
    Skipped,
}

#[derive(Debug, Serialize)]
struct Check {
    id: String,
    resource: Value,
    status: Status,
    reason_code: String,
    message: String,
    evidence: Value,
    remediations: Vec<Value>,
}

#[derive(Serialize)]
struct Report {
    schema_version: u32,
    nook_version: &'static str,
    mode: &'static str,
    result: &'static str,
    complete: bool,
    exit_code: i32,
    context: Value,
    duration_ms: u128,
    counts: BTreeMap<String, usize>,
    checks: Vec<Check>,
    #[serde(skip)]
    deadline: Instant,
}

impl Report {
    fn new(deep: bool, timeout: Duration) -> Self {
        Self {
            schema_version: 1,
            nook_version: env!("CARGO_PKG_VERSION"),
            mode: if deep { "deep_active" } else { "quick" },
            result: "passed",
            complete: true,
            exit_code: 0,
            context: json!({"platform": std::env::consts::OS, "snapshot": "best_effort", "traffic_effects_possible": deep}),
            duration_ms: 0,
            counts: BTreeMap::new(),
            checks: Vec::new(),
            deadline: Instant::now() + timeout,
        }
    }
    fn add(
        &mut self,
        id: &str,
        resource: Value,
        status: Status,
        reason: &str,
        message: impl Into<String>,
        evidence: Value,
    ) {
        self.checks.push(Check {
            id: id.into(),
            resource,
            status,
            reason_code: reason.into(),
            message: message.into(),
            evidence,
            remediations: Vec::new(),
        });
    }
    fn simple(&mut self, id: &str, status: Status, reason: &str, message: impl Into<String>) {
        self.add(id, Value::Null, status, reason, message, Value::Null);
    }
    fn suggestion(&mut self, argv: &[&str], precondition: &str) {
        if let Some(check) = self.checks.last_mut() {
            check.remediations.push(json!({"id": argv.join("_"), "argv": argv, "mutating": true, "requires_privilege": false, "precondition": precondition}));
        }
    }
    fn budget(&mut self, id: &str, cap: Duration) -> Option<Duration> {
        match self.deadline.checked_duration_since(Instant::now()) {
            Some(left) if !left.is_zero() => Some(left.min(cap)),
            _ => {
                self.complete = false;
                self.simple(
                    id,
                    Status::Skipped,
                    "budget_exhausted",
                    "Total observation budget exhausted",
                );
                None
            }
        }
    }
    fn probe(&mut self, id: &str, probe: doctor_io::Probe, cap: u64) -> Option<io::Result<Value>> {
        let budget = self.budget(id, Duration::from_millis(cap))?;
        let result = doctor_io::probe(probe, budget);
        if result
            .as_ref()
            .is_err_and(|error| error.kind() == io::ErrorKind::TimedOut)
        {
            self.complete = false;
        }
        Some(result)
    }
    fn finish(&mut self) {
        let uncertain = !self.complete
            || self.checks.iter().any(|check| {
                matches!(
                    check.id.as_str(),
                    "caddy.ownership" | "caddy.foreign" | "caddy.container" | "state.ownership"
                ) && check.status != Status::Pass
            });
        if uncertain {
            for check in &mut self.checks {
                check.remediations.clear();
            }
        }
        self.checks
            .sort_by_key(|c| (c.id.clone(), c.resource.to_string()));
        self.counts.clear();
        for check in &self.checks {
            *self
                .counts
                .entry(format!("{:?}", check.status).to_lowercase())
                .or_default() += 1;
        }
        self.exit_code = if self.checks.iter().any(|c| c.status == Status::Fail) {
            1
        } else if !self.complete {
            3
        } else {
            0
        };
        self.result = match self.exit_code {
            1 => "failed",
            3 => "incomplete",
            _ if self.checks.iter().any(|c| c.status == Status::Warning) => "warning",
            _ => "passed",
        };
    }
}

pub(crate) fn usage_error(output: &mut impl Write) -> io::Result<()> {
    let mut report = Report::new(false, Duration::from_secs(1));
    report.simple(
        "cli.usage",
        Status::Fail,
        "invalid_arguments",
        "Invalid doctor arguments; use nook doctor --help",
    );
    report.finish();
    report.exit_code = 2;
    serde_json::to_writer(&mut *output, &report)?;
    writeln!(output)
}

pub(crate) fn execute(
    args: Args,
    socket: Option<String>,
    output: &mut impl Write,
) -> crate::Result<i32> {
    let started = Instant::now();
    let mut report = Report::new(
        args.deep,
        args.timeout
            .unwrap_or(Duration::from_secs(if args.deep { 30 } else { 10 })),
    );
    let _deadline = doctor_io::Deadline::enter(report.deadline);
    let global = match config::load_global() {
        Ok(mut global) => {
            if let Some(socket) = socket {
                global.caddy_admin = format!("unix/{socket}");
            }
            report.add("config.global", Value::Null, Status::Pass, "loaded", "Global configuration loaded", json!({"path": config::global_config_path().ok(), "run_bind_address": global.run_bind_address, "caddy_loopback_host": global.caddy_loopback_host, "caddy_client_ip_ranges": global.caddy_client_ip_ranges}));
            Some(global)
        }
        Err(error) => {
            report.simple(
                "config.global",
                Status::Fail,
                "invalid_config",
                safe_config_error(&error),
            );
            None
        }
    };
    let store = state::state_path().map(state::Store::new);
    let mut original = None;
    let registry = match &store {
        Ok(store) => match store.inspect() {
            Ok((registry, bytes)) => {
                report.add(
                    "state.snapshot",
                    Value::Null,
                    Status::Pass,
                    if registry.is_some() {
                        "loaded"
                    } else {
                        "not_initialized"
                    },
                    "State read without creating files; write permission is not proven",
                    json!({"path": store.path(), "present": registry.is_some()}),
                );
                original = Some(bytes);
                registry
            }
            Err(error) => {
                let busy = matches!(&error, state::Error::Io { source, .. } if source.kind() == io::ErrorKind::WouldBlock);
                if busy {
                    report.complete = false;
                }
                report.simple(
                    "state.snapshot",
                    if busy { Status::Warning } else { Status::Fail },
                    if busy {
                        "lock_busy"
                    } else {
                        "unreadable_state"
                    },
                    error.to_string(),
                );
                None
            }
        },
        Err(error) => {
            report.simple(
                "state.snapshot",
                Status::Fail,
                "unknown_path",
                error.to_string(),
            );
            None
        }
    };
    if let Some(registry) = &registry {
        inspect_state(&mut report, registry, args.owner);
    }
    let desired = if args.owner.is_none() {
        inspect_project(&mut report, &args, global.as_ref())
    } else {
        Vec::new()
    };
    if let Some(owner) = args.owner
        && !registry.as_ref().is_some_and(|r| {
            r.leases.contains_key(&owner) || r.aliases.values().any(|a| a.id == owner)
        })
    {
        report.simple(
            "state.owner",
            Status::Fail,
            "owner_not_found",
            "Requested owner is not present in the readable registry",
        );
    }
    let specs: Vec<_> = registry
        .as_ref()
        .into_iter()
        .flat_map(|r| {
            r.leases
                .values()
                .map(reconcile::route_for_lease)
                .chain(r.aliases.values().map(reconcile::route_for_alias))
        })
        .filter(|r| args.owner.is_none_or(|id| id == r.owner_id))
        .collect();
    let mut observed = None;
    if let Some(global) = &global {
        if let Some(result) = report.probe(
            "caddy.api",
            doctor_io::Probe::Caddy(global.caddy_admin.clone()),
            2000,
        ) {
            match result {
                Ok(value) => {
                    report.simple(
                        "caddy.api",
                        Status::Pass,
                        "readable",
                        "Caddy config readable; mutation permission not tested",
                    );
                    inspect_routes(
                        &mut report,
                        global,
                        &value["config"],
                        &specs,
                        registry.is_some(),
                        &desired,
                    );
                    observed = Some(value);
                }
                Err(error) => {
                    report.simple("caddy.api", Status::Fail, "unreachable", error.to_string());
                    report.simple(
                        "caddy.routes",
                        Status::Skipped,
                        "caddy_unavailable",
                        "Route checks require a readable Caddy config",
                    );
                }
            }
        }
        inspect_network(&mut report, global, &desired, &specs);
    } else {
        report.simple(
            "caddy.api",
            Status::Skipped,
            "invalid_global_config",
            "Caddy address cannot be determined safely",
        );
    }
    inspect_ca(&mut report, args.ca_cert.as_deref());
    let tailscale_required = desired.iter().any(|c| c.tailscale)
        || registry
            .as_ref()
            .is_some_and(|r| !r.tailscale.registrations.is_empty());
    if tailscale_required {
        let deadline = report.deadline;
        let complete = crate::tailscale::doctor(
            &mut |id, status, reason, message, evidence| {
                report.add(id, Value::Null, status, reason, message, evidence)
            },
            registry.as_ref(),
            deadline,
        );
        report.complete &= complete;
    } else {
        report.simple(
            "tailscale",
            Status::Skipped,
            "not_configured",
            "No Tailscale exposure requested or recorded",
        );
    }
    if args.deep {
        deep_checks(&mut report, &specs, args.ca_cert.as_deref());
    } else {
        report.simple(
            "route.probes",
            Status::Skipped,
            "quick_mode",
            "DNS/TCP/TLS/HTTP/mDNS traffic requires --deep",
        );
    }
    if let (Ok(store), Some(original)) = (&store, original) {
        match store.inspect() {
            Ok((_, current)) if current == original => {}
            _ => {
                report.complete = false;
                report.simple(
                    "snapshot.state",
                    Status::Warning,
                    "snapshot_changed",
                    "State changed or became unavailable during inspection; rerun doctor",
                );
            }
        }
    }
    if let (Some(global), Some(before)) = (&global, observed)
        && let Some(result) = report.probe(
            "snapshot.caddy",
            doctor_io::Probe::Caddy(global.caddy_admin.clone()),
            2000,
        )
    {
        match result {
            Ok(after) if after == before => report.simple(
                "snapshot.caddy",
                Status::Pass,
                "unchanged",
                "Caddy config unchanged across observations",
            ),
            _ => {
                report.complete = false;
                report.simple(
                    "snapshot.caddy",
                    Status::Warning,
                    "snapshot_changed",
                    "Caddy changed or became unavailable; rerun doctor",
                );
            }
        }
    }
    report.duration_ms = started.elapsed().as_millis();
    report.finish();
    if args.json {
        serde_json::to_writer(&mut *output, &report).map_err(io::Error::other)?;
        writeln!(output)?;
    } else {
        render(&report, output)?;
    }
    Ok(report.exit_code)
}

fn safe_config_error(error: &config::Error) -> String {
    // TOML parser diagnostics may echo secrets from the offending source line.
    match error {
        config::Error::Parse { path, .. } => format!(
            "Invalid configuration at {}; check TOML, fields and types",
            path.display()
        ),
        _ => error.to_string(),
    }
}

fn inspect_project(
    report: &mut Report,
    args: &Args,
    global: Option<&config::GlobalConfig>,
) -> Vec<config::ResolvedRunConfig> {
    let Ok(cwd) = std::env::current_dir() else {
        report.simple(
            "config.project",
            Status::Fail,
            "cwd_unavailable",
            "Cannot read working directory",
        );
        return Vec::new();
    };
    let explicit = args.config.is_some()
        || args.name.is_some()
        || !args.command.is_empty()
        || args.runner
        || args.worktree.is_some()
        || args.only.is_some();
    if !explicit && !cwd.join("nook.toml").exists() && !cwd.join("nook.local.toml").exists() {
        report.simple(
            "config.project",
            Status::Skipped,
            "no_project",
            "No project configuration in the current directory",
        );
        return Vec::new();
    }
    let run = crate::cli::RunArgs {
        observation: false,
        config: args.config.clone(),
        local: args.local,
        name: args.name.clone(),
        command: args.command.clone(),
        worktree: args.worktree.clone(),
        no_worktree: args.no_worktree,
        only: args.only.clone(),
        runner: args.runner,
        no_tls: args.no_tls,
        tailscale: args.tailscale,
        funnel: args.funnel,
        no_tailscale: args.no_tailscale,
        lan: args.lan,
        no_lan: args.no_lan,
        run_bind_address: args.run_bind_address,
        app_port: args.app_port,
        strict_port: args.strict_port,
        readiness_warn_after: args.readiness_warn_after,
        framework: args.framework.clone(),
        no_framework: args.no_framework,
        force: false,
    };
    let bind = global.map_or_else(
        || config::GlobalConfig::default().run_bind_address,
        |g| g.run_bind_address,
    );
    let result =
        crate::workspace_config::load(&run, &cwd, bind).and_then(|workspace| match workspace {
            Some(workspace) => {
                if args
                    .only
                    .as_ref()
                    .is_some_and(|id| !workspace.apps.iter().any(|a| &a.id == id))
                {
                    return Err(io::Error::other("--only does not match a workspace app").into());
                }
                Ok(workspace
                    .apps
                    .into_iter()
                    .filter(|app| args.only.as_ref().is_none_or(|id| id == &app.id))
                    .map(|a| a.config)
                    .collect::<Vec<_>>())
            }
            None if args.runner || args.only.is_some() => {
                Err(io::Error::other("--runner/--only requires a workspace manifest").into())
            }
            None => config::resolve_run(&run, &cwd, bind)
                .map(|c| vec![c])
                .map_err(Into::into),
        });
    match result {
        Ok(configs) => {
            for config in &configs {
                if config.app_port == Some(0) {
                    report.simple(
                        "config.port",
                        Status::Fail,
                        "invalid_port",
                        "Application port must be between 1 and 65535",
                    );
                }
                if config.tailscale
                    && let Err(error) =
                        crate::tailscale::reachable_bind_address(config.bind_address)
                {
                    report.simple(
                        "config.tailscale_bind",
                        Status::Fail,
                        "unreachable_bind",
                        error.to_string(),
                    );
                }
                report.add("config.project", json!({"hostname": config.hostname, "worktree": config.worktree}), Status::Pass, "resolved", "Resolved launch configuration; no command executed or port reserved", json!({"bind": config.bind_address, "tls": config.tls, "lan": config.lan, "tailscale": config.tailscale, "funnel": config.funnel, "app_port": config.app_port, "strict_port": config.strict_port, "readiness_warn_after_seconds": config.readiness_warn_after_seconds, "ignored_local_config": config.ignored_local_config, "provenance": config.provenance, "config": args.config.as_ref().unwrap_or(&cwd.join("nook.toml"))}));
            }
            configs
        }
        Err(error) => {
            let message = match &error {
                crate::Error::Config(error) => safe_config_error(error),
                _ => error.to_string(),
            };
            report.simple("config.project", Status::Fail, "invalid_project", message);
            Vec::new()
        }
    }
}

fn inspect_state(report: &mut Report, registry: &state::Registry, selected: Option<Uuid>) {
    if !registry.pending_operations.is_empty() {
        report.add("state.pending", Value::Null, Status::Warning, "pending_operations", "Recovery operations remain pending; inspection does not replay them", json!({"count": registry.pending_operations.len(), "last_synchronized_at_unix_ms": registry.last_synchronized_at_unix_ms}));
    }
    let mut owners = BTreeSet::new();
    for id in registry
        .leases
        .keys()
        .copied()
        .chain(registry.aliases.values().map(|a| a.id))
    {
        if !owners.insert(id) {
            report.simple(
                "state.ownership",
                Status::Fail,
                "duplicate_owner",
                format!("Duplicate owner UUID {id}"),
            );
        }
    }
    for lease in registry
        .leases
        .values()
        .filter(|l| selected.is_none_or(|id| id == l.id))
    {
        if registry.leases.get(&lease.id) != Some(lease) {
            report.simple(
                "state.ownership",
                Status::Fail,
                "lease_key_mismatch",
                "Lease key does not match its owner UUID; no cleanup authorized",
            );
        }
        let supervisor = lease
            .workspace_supervisor
            .as_ref()
            .map(process::workspace_supervisor_liveness);
        let live = process::lease_liveness(lease);
        let (status, reason) = match (live, supervisor) {
            (_, Some(process::Liveness::Dead)) => (Status::Fail, "supervisor_dead"),
            (process::Liveness::Dead, Some(process::Liveness::Alive)) => {
                (Status::Warning, "runner_member_restarting")
            }
            (process::Liveness::Dead, _) => (Status::Fail, "process_dead_or_reused"),
            (process::Liveness::Indeterminate, _) | (_, Some(process::Liveness::Indeterminate)) => {
                (Status::Warning, "identity_indeterminate")
            }
            _ => (Status::Pass, "identity_alive"),
        };
        report.add("process.identity", json!({"owner_id": lease.id, "hostname": lease.hostname, "worktree": lease.worktree, "group_id": lease.group_id}), status, reason, "Observed process identity; recorded readiness is not an HTTP health check", json!({"pid": lease.pid, "recorded_state": lease.state}));
        if lease
            .group_id
            .is_some_and(|id| !registry.workspace_groups.contains_key(&id))
        {
            report.simple(
                "workspace.group",
                Status::Warning,
                "group_missing",
                "Lease refers to an absent workspace group",
            );
        }
    }
    for (id, group) in &registry.workspace_groups {
        if selected.is_some_and(|owner| {
            !registry
                .leases
                .get(&owner)
                .is_some_and(|l| l.group_id == Some(*id))
        }) {
            continue;
        }
        let alive = process::workspace_supervisor_liveness(&group.supervisor);
        let path = group.session_path.as_ref().map(PathBuf::from);
        report.add("workspace.session", json!({"group_id": id}), if alive == process::Liveness::Alive {Status::Pass} else {Status::Warning}, "session_observation", "Session is never joined or cleaned by doctor", json!({"supervisor_alive": alive == process::Liveness::Alive, "path": path, "path_exists": path.as_ref().is_some_and(|p| p.exists())}));
        match crate::workspace_session::inspect(*id, group) {
            Ok(summary) => report.add(
                "workspace.context",
                json!({"group_id": id}),
                Status::Pass,
                "context_readable",
                "Session identity and members inspected without joining",
                summary,
            ),
            Err(_) => report.add(
                "workspace.context",
                json!({"group_id": id}),
                Status::Warning,
                "context_unavailable",
                "Session busy, absent, invalid or not private; no cleanup authorized",
                Value::Null,
            ),
        }
    }
}

fn inspect_ca(report: &mut Report, path: Option<&std::path::Path>) {
    let Some(path) = path else {
        report.simple("ca.trust", Status::Skipped, "certificate_not_supplied", "Use --ca-cert with an existing public PEM; PKI GETs can provision a CA and are not called");
        return;
    };
    let result = std::fs::read_to_string(path)
        .map_err(crate::Error::from)
        .and_then(|pem| {
            let (_, der) = caddy::canonical_local_ca(&pem)?;
            Ok((
                Sha256::digest(der)
                    .iter()
                    .map(|byte| format!("{byte:02x}"))
                    .collect::<String>(),
                caddy::inspect_local_ca_is_trusted(&pem),
            ))
        });
    match result {
        Ok((fingerprint, Ok(trusted))) => report.add("ca.trust", Value::Null, if trusted {Status::Pass} else {Status::Warning}, if trusted {"supplied_certificate_found"} else {"supplied_certificate_not_found"}, "Supplied certificate only; association with active Caddy and browser trust are not proven", json!({"sha256": fingerprint, "trusted_in_inspected_stores": trusted})),
        Ok((fingerprint, Err(_))) => report.add("ca.trust", Value::Null, Status::Warning, "trust_store_unreadable", "Cannot inspect all relevant trust stores; absence is not established", json!({"sha256": fingerprint})),
        Err(_) => report.simple("ca.trust", Status::Fail, "certificate_unreadable", "Cannot read or parse the supplied public CA certificate"),
    }
}

fn inspect_network(
    report: &mut Report,
    global: &config::GlobalConfig,
    desired: &[config::ResolvedRunConfig],
    specs: &[reconcile::RouteSpec],
) {
    let addresses =
        if_addrs::get_if_addrs().map(|v| v.into_iter().map(|i| i.ip()).collect::<Vec<_>>());
    for bind in std::iter::once(global.run_bind_address)
        .chain(desired.iter().map(|c| c.bind_address))
        .collect::<BTreeSet<_>>()
    {
        let present =
            bind.is_unspecified() || addresses.as_ref().is_ok_and(|ips| ips.contains(&bind));
        report.add(
            "network.bind",
            json!({"address": bind}),
            if present {
                if bind.is_unspecified() {
                    Status::Warning
                } else {
                    Status::Pass
                }
            } else {
                if addresses.is_ok() && desired.iter().any(|config| config.bind_address == bind) {
                    Status::Fail
                } else {
                    Status::Warning
                }
            },
            if present {
                "local_bind"
            } else {
                "interface_not_observed"
            },
            "Application bind viewed from this host; no port reserved",
            json!({"wildcard": bind.is_unspecified()}),
        );
    }
    if global.caddy_loopback_host != "127.0.0.1" {
        report.simple("network.caddy_upstream", Status::Warning, "different_network_vantage", "Verify run_bind_address, caddy_loopback_host and caddy_client_ip_ranges together; host reachability does not prove Caddy/container reachability");
    }
    if desired.iter().any(|c| c.lan) || specs.iter().any(|r| r.lan) {
        match crate::lan::addresses() {
            Ok(addresses) if !addresses.is_empty() => report.add(
                "lan.interfaces",
                Value::Null,
                Status::Pass,
                "ipv4_present",
                "LAN IPv4 interface observed; remote firewall reachability is unproven",
                json!({"addresses": addresses}),
            ),
            _ => report.simple(
                "lan.interfaces",
                Status::Fail,
                "no_lan_ipv4",
                "LAN mode requires an active non-loopback IPv4 interface",
            ),
        }
    } else {
        report.simple(
            "lan",
            Status::Skipped,
            "not_configured",
            "LAN is not requested",
        );
    }
}

fn inspect_routes(
    report: &mut Report,
    global: &config::GlobalConfig,
    config: &Value,
    specs: &[reconcile::RouteSpec],
    state_known: bool,
    desired: &[config::ResolvedRunConfig],
) {
    let Some(servers) = config
        .pointer("/apps/http/servers")
        .and_then(Value::as_object)
    else {
        report.simple(
            "caddy.servers",
            Status::Fail,
            "servers_missing",
            "apps.http.servers is absent or invalid",
        );
        return;
    };
    let mut selected = BTreeMap::new();
    for tls in [false, true] {
        let required = specs.iter().any(|s| s.tls == tls || (!tls && s.lan))
            || desired.iter().any(|s| s.tls == tls || (!tls && s.lan));
        let result = caddy::discover_servers(
            config,
            caddy::ServerOverrides {
                https: global.https_server.as_deref(),
                http: global.http_server.as_deref(),
            },
            tls,
            !tls,
        );
        match result {
            Ok(selection) => {
                let name = if tls { selection.https } else { selection.http }.unwrap();
                selected.insert(tls, name.clone());
                report.add(
                    "caddy.server",
                    json!({"tls": tls}),
                    Status::Pass,
                    "selected",
                    "Server selected",
                    json!({"server": name}),
                );
            }
            Err(error) => {
                report.add(
                    "caddy.server",
                    json!({"tls": tls}),
                    if required {
                        Status::Fail
                    } else {
                        Status::Warning
                    },
                    "selection_unavailable",
                    error.to_string(),
                    Value::Null,
                );
            }
        }
    }
    let containers = [
        caddy::HTTPS_CONTAINER_ID,
        caddy::HTTP_CONTAINER_ID,
        caddy::LAN_CONTAINER_ID,
    ];
    let mut observed: BTreeMap<String, Vec<(&str, &str, &Value)>> = BTreeMap::new();
    for (server_name, server) in servers {
        let Some(routes) = server.get("routes").and_then(Value::as_array) else {
            continue;
        };
        let mut seen_containers = BTreeSet::new();
        for (index, container) in routes.iter().enumerate() {
            let Some(id) = container["@id"]
                .as_str()
                .filter(|id| containers.contains(id))
            else {
                continue;
            };
            if !seen_containers.insert(id) {
                report.simple(
                    "caddy.ownership",
                    Status::Fail,
                    "duplicate_container",
                    format!("Duplicate {id} on {server_name}"),
                );
            }
            if routes[..index].iter().any(|r| {
                r.get("match")
                    .and_then(Value::as_array)
                    .is_none_or(Vec::is_empty)
            }) {
                report.simple(
                    "caddy.order",
                    Status::Warning,
                    "preceding_catch_all",
                    format!("Catch-all precedes {id} on {server_name}"),
                );
            }
            let Some(children) = container
                .pointer("/handle/0/routes")
                .and_then(Value::as_array)
            else {
                report.simple(
                    "caddy.ownership",
                    Status::Fail,
                    "malformed_container",
                    format!("Malformed {id} on {server_name}"),
                );
                continue;
            };
            let members: Vec<_> = children
                .iter()
                .filter_map(|route| {
                    Some(caddy::ManagedRoute {
                        hostname: route.pointer("/match/0/host/0")?.as_str()?.into(),
                        no_tls: id != caddy::HTTPS_CONTAINER_ID,
                        route: route.clone(),
                    })
                })
                .collect();
            if caddy::build_container(id, members.iter()).as_ref() != Some(container) {
                report.simple("caddy.container", Status::Fail, "container_differs", format!("Container {id} on {server_name} has unexpected matchers, handlers or children"));
            }
            for route in children {
                let Some(route_id) = route["@id"]
                    .as_str()
                    .filter(|id| caddy::parse_owner_route_id(id).is_some())
                else {
                    report.simple(
                        "caddy.ownership",
                        Status::Fail,
                        "unrecognized_owner",
                        format!(
                            "Unrecognized child in {id} on {server_name}; do not prune blindly"
                        ),
                    );
                    continue;
                };
                observed
                    .entry(route_id.into())
                    .or_default()
                    .push((server_name, id, route));
            }
        }
    }
    let mut expected_ids = BTreeSet::new();
    let mut all_ids = BTreeMap::new();
    collect_route_ids(&Value::Object(servers.clone()), &mut all_ids);
    for (id, count) in all_ids {
        if count != observed.get(&id).map_or(0, Vec::len) {
            report.add("caddy.ownership", json!({"route_id": id}), Status::Fail, "owner_outside_container", "Owner ID occurs outside a recognized Nook container; no adoption or cleanup authorized", json!({"count": count}));
        }
    }
    for spec in specs {
        for lan in [false, true].into_iter().filter(|lan| !lan || spec.lan) {
            let hostname = if lan {
                crate::lan::hostname(&spec.hostname)
            } else {
                spec.hostname.clone()
            };
            let id = if lan {
                format!("nook_lan_route_v1_{}", spec.owner_id)
            } else {
                caddy::owner_route_id(spec.owner_id)
            };
            expected_ids.insert(id.clone());
            let container = if lan {
                caddy::LAN_CONTAINER_ID
            } else if spec.tls {
                caddy::HTTPS_CONTAINER_ID
            } else {
                caddy::HTTP_CONTAINER_ID
            };
            let resource = json!({"owner_id": spec.owner_id, "hostname": hostname});
            let Some(server) = selected.get(&(spec.tls && !lan)) else {
                report.add(
                    "caddy.route",
                    resource,
                    Status::Skipped,
                    "server_unavailable",
                    "Required server selection failed",
                    Value::Null,
                );
                continue;
            };
            if lan && !lan_listener(&servers[server]) {
                report.add("lan.listener", resource.clone(), Status::Fail, "loopback_only_listener", "LAN requires an HTTP listener on :80 or a non-loopback IPv4 address; adjust Caddy explicitly", Value::Null);
            }
            let target = match caddy::normalize_upstream(&spec.target) {
                Ok(target) => target,
                Err(_) => {
                    report.add(
                        "caddy.route",
                        resource,
                        Status::Fail,
                        "invalid_recorded_target",
                        "Recorded upstream is invalid",
                        Value::Null,
                    );
                    continue;
                }
            };
            let ranges = if lan {
                vec!["0.0.0.0/0".into()]
            } else {
                global.caddy_client_ip_ranges.clone()
            };
            let expected = caddy::build_proxy_route_for_network(
                &id,
                &hostname,
                &target.url,
                spec.preserve_host,
                &global.caddy_loopback_host,
                &ranges,
            );
            let rows = observed.get(&id);
            let correct = rows.is_some_and(|rows| {
                rows.len() == 1
                    && rows[0].0 == server
                    && rows[0].1 == container
                    && rows[0].2 == &expected
            });
            report.add("caddy.route", resource.clone(), if correct {Status::Pass} else {Status::Fail}, if correct {"matches"} else if rows.is_none() {"expected_route_absent"} else {"route_differs"}, if correct {"Owned route matches recorded intent"} else {"Owned route missing, moved, duplicated or changed"}, json!({"server": server, "container": container, "matches": rows.map_or(0, Vec::len)}));
            if !correct {
                report.suggestion(
                    &["nook", "prune"],
                    "First verify unambiguous ownership; prune can clean dead resources globally",
                );
            }
            if let Some(routes) = servers[server]["routes"].as_array() {
                inspect_foreign_hostname(report, routes, &hostname, resource);
            }
        }
    }
    // Hypothetical launches need collision checks, but do not yet own routes.
    let mut desired_hosts = BTreeSet::new();
    for config in desired {
        desired_hosts.insert((config.tls, config.hostname.clone(), false));
        if config.lan {
            desired_hosts.insert((false, crate::lan::hostname(&config.hostname), true));
        }
    }
    for (tls, hostname, lan) in desired_hosts {
        let Some(server) = selected.get(&tls) else {
            continue;
        };
        let resource = json!({"hostname": hostname, "simulated": true});
        if lan && !lan_listener(&servers[server]) {
            report.add("lan.listener", resource.clone(), Status::Fail, "loopback_only_listener", "LAN requires an HTTP listener on :80 or a non-loopback IPv4 address; adjust Caddy explicitly", Value::Null);
        }
        if let Some(routes) = servers[server]["routes"].as_array() {
            inspect_foreign_hostname(report, routes, &hostname, resource);
        }
    }
    for (id, rows) in observed {
        if state_known && !expected_ids.contains(&id) {
            report.add("caddy.unmatched", json!({"route_id": id}), Status::Warning, "outside_selected_owners", "Route has no owner in the selected snapshot; it may belong to another selection. No deletion authorized", json!({"count": rows.len()}));
        }
    }
    if !state_known {
        report.simple(
            "caddy.ownership",
            Status::Skipped,
            "state_unavailable",
            "Cannot establish route ownership without a registry; no deletion authorized",
        );
    }
}

fn inspect_foreign_hostname(
    report: &mut Report,
    routes: &[Value],
    hostname: &str,
    resource: Value,
) {
    if caddy::reject_foreign_hostname_claims(routes, &[hostname.to_owned()]).is_err() {
        report.add("caddy.foreign", resource, Status::Fail, "foreign_hostname", "Foreign route claims this hostname; choose another name or edit Caddy explicitly, never force ownership", Value::Null);
    } else if routes
        .iter()
        .filter(|route| {
            !route["@id"].as_str().is_some_and(|id| {
                [
                    caddy::HTTPS_CONTAINER_ID,
                    caddy::HTTP_CONTAINER_ID,
                    caddy::LAN_CONTAINER_ID,
                ]
                .contains(&id)
            })
        })
        .any(|route| route.to_string().contains('*'))
    {
        report.add(
            "caddy.foreign",
            resource,
            Status::Warning,
            "complex_matcher",
            "Foreign wildcard configuration exists; overlap is not fully evaluated",
            Value::Null,
        );
    }
}

fn lan_listener(server: &Value) -> bool {
    server["listen"].as_array().is_some_and(|listeners| {
        listeners.iter().filter_map(Value::as_str).any(|address| {
            let address = address
                .strip_prefix("tcp/")
                .or_else(|| address.strip_prefix("tcp4/"))
                .unwrap_or(address);
            address.strip_suffix(":80").is_some_and(|host| {
                host.is_empty()
                    || host == "[::]"
                    || host
                        .parse::<std::net::Ipv4Addr>()
                        .is_ok_and(|ip| !ip.is_loopback())
            })
        })
    })
}

fn collect_route_ids(value: &Value, ids: &mut BTreeMap<String, usize>) {
    match value {
        Value::Object(object) => {
            if let Some(id) = object
                .get("@id")
                .and_then(Value::as_str)
                .filter(|id| caddy::parse_owner_route_id(id).is_some())
            {
                *ids.entry(id.into()).or_default() += 1;
            }
            for child in object.values() {
                collect_route_ids(child, ids);
            }
        }
        Value::Array(array) => {
            for child in array {
                collect_route_ids(child, ids);
            }
        }
        _ => {}
    }
}

fn deep_checks(
    report: &mut Report,
    specs: &[reconcile::RouteSpec],
    ca_cert: Option<&std::path::Path>,
) {
    if specs.is_empty() {
        report.simple(
            "route.probes",
            Status::Skipped,
            "no_recorded_resources",
            "No recorded routes to probe; hypothetical applications are never started",
        );
    }
    for spec in specs {
        let resource = json!({"owner_id": spec.owner_id, "hostname": spec.hostname});
        if let Some(result) = report.probe(
            "dns.localhost",
            doctor_io::Probe::Dns(spec.hostname.clone()),
            2000,
        ) {
            match result {
                Ok(ips) => {
                    let loopback = ips.as_array().is_some_and(|ips| {
                        !ips.is_empty()
                            && ips.iter().all(|ip| {
                                ip.as_str()
                                    .and_then(|ip| ip.parse::<IpAddr>().ok())
                                    .is_some_and(|ip| ip.is_loopback())
                            })
                    });
                    report.add(
                        "dns.localhost",
                        resource.clone(),
                        if loopback { Status::Pass } else { Status::Fail },
                        "system_resolution",
                        "System resolver observation; browsers may use a different resolver",
                        ips,
                    );
                }
                Err(_) => report.add(
                    "dns.localhost",
                    resource.clone(),
                    Status::Warning,
                    "resolver_unavailable",
                    "System resolver did not resolve this .localhost name",
                    Value::Null,
                ),
            }
        }
        if let Ok(target) = caddy::normalize_upstream(&spec.target)
            && let Some(result) = report.probe(
                "upstream.tcp",
                doctor_io::Probe::Tcp(
                    target.url.host_str().unwrap().into(),
                    target.url.port_or_known_default().unwrap(),
                ),
                2000,
            )
        {
            report.add(
                "upstream.tcp",
                resource.clone(),
                if result.is_ok() {
                    Status::Pass
                } else {
                    Status::Warning
                },
                "host_vantage",
                "TCP reachability from Nook, not from Caddy; not application health",
                result.unwrap_or(Value::Null),
            );
        }
        let url = format!(
            "{}://{}/",
            if spec.tls { "https" } else { "http" },
            spec.hostname
        );
        for address in [
            None,
            Some("127.0.0.1".parse().unwrap()),
            Some("::1".parse().unwrap()),
        ] {
            if let Some(result) = report.probe(
                "route.http",
                doctor_io::Probe::Http {
                    url: url.clone(),
                    address,
                    ca_cert: ca_cert.map(PathBuf::from),
                },
                3000,
            ) {
                match result {
                Ok(value) => { let code = value["status"].as_u64().unwrap_or(0); report.add("route.http", resource.clone(), if (200..400).contains(&code) {Status::Pass} else {Status::Warning}, "http_response", "HEAD response received; handler ownership and application health are not proven", value); }
                Err(_) => report.add("route.http", json!({"owner_id": spec.owner_id, "hostname": spec.hostname, "address": address}), if address.is_some() {Status::Warning} else {Status::Fail}, "transport_or_tls_failed", "HTTP transport or TLS validation failed; an unused IP family is not a broken route", Value::Null),
            }
            }
        }
        if spec.lan
            && let Some(result) = report.probe(
                "lan.mdns",
                doctor_io::Probe::Mdns(crate::lan::hostname(&spec.hostname)),
                2000,
            )
        {
            let value = result.unwrap_or(Value::Null);
            report.add(
                "lan.mdns",
                resource,
                if value["observed"] == true {
                    Status::Pass
                } else {
                    Status::Warning
                },
                "local_observation",
                "mDNS observation from this host; verify accessibility from another LAN device",
                value,
            );
        }
    }
}

fn render(report: &Report, output: &mut impl Write) -> io::Result<()> {
    writeln!(output, "Nook doctor — {} — no reconciliation", report.mode)?;
    if report.mode == "deep_active" {
        writeln!(
            output,
            "Active traffic may trigger application logs or Caddy certificate issuance."
        )?;
    }
    for status in [Status::Fail, Status::Warning, Status::Skipped, Status::Pass] {
        for check in report.checks.iter().filter(|c| c.status == status) {
            writeln!(
                output,
                "{:?}\t{}\t{}\t{}",
                status, check.id, check.resource, check.message
            )?;
            for action in &check.remediations {
                writeln!(
                    output,
                    "  Suggested: {} ({})",
                    action["argv"], action["precondition"]
                )?;
            }
        }
    }
    writeln!(
        output,
        "Result: {} — complete={} — exit={}",
        report.result, report.complete, report.exit_code
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn route_fixture() -> (config::GlobalConfig, reconcile::RouteSpec, Value) {
        let global = config::GlobalConfig::default();
        let spec = reconcile::RouteSpec {
            lan: false,
            owner_id: Uuid::new_v4(),
            hostname: "app.localhost".into(),
            target: "http://127.0.0.1:3000".into(),
            scheme: state::Scheme::Http,
            tls: false,
            replace_existing: false,
            preserve_host: false,
        };
        let route = caddy::build_proxy_route_for_network(
            &caddy::owner_route_id(spec.owner_id),
            &spec.hostname,
            &url::Url::parse(&spec.target).unwrap(),
            false,
            &global.caddy_loopback_host,
            &global.caddy_client_ip_ranges,
        );
        let members = [caddy::ManagedRoute {
            hostname: spec.hostname.clone(),
            no_tls: true,
            route,
        }];
        let container = caddy::build_container(caddy::HTTP_CONTAINER_ID, members.iter()).unwrap();
        let config = json!({"apps":{"http":{"servers":{"http":{"listen":[":80"],"routes":[container]},"https":{"listen":[":443"],"routes":[]}}}}});
        (global, spec, config)
    }

    #[test]
    fn drift_compares_owned_payload_and_container_not_only_owner_and_hostname() {
        let (global, spec, original) = route_fixture();
        for mutation in [
            "none",
            "upstream",
            "host",
            "container",
            "duplicate",
            "missing",
        ] {
            let mut config = original.clone();
            let routes = &mut config["apps"]["http"]["servers"]["http"]["routes"];
            match mutation {
                "upstream" => {
                    routes[0]["handle"][0]["routes"][0]["handle"][0]["upstreams"][0]["dial"] =
                        json!("127.0.0.1:4000")
                }
                "host" => {
                    routes[0]["handle"][0]["routes"][0]["handle"][0]["headers"]["request"]["set"]
                        ["Host"] = json!(["foreign.example"])
                }
                "container" => routes[0]["match"][0]["host"] = json!(["wrong.localhost"]),
                "duplicate" => {
                    let copy = routes[0].clone();
                    routes.as_array_mut().unwrap().push(copy);
                }
                "missing" => *routes = json!([]),
                _ => {}
            }
            let before = config.clone();
            let mut report = Report::new(false, Duration::from_secs(1));
            inspect_routes(
                &mut report,
                &global,
                &config,
                std::slice::from_ref(&spec),
                true,
                &[],
            );
            report.finish();
            assert_eq!(
                report.exit_code,
                if mutation == "none" { 0 } else { 1 },
                "{mutation}"
            );
            assert_eq!(config, before);
        }
    }

    #[test]
    fn foreign_conflict_never_gets_a_force_remediation() {
        let (global, spec, mut config) = route_fixture();
        config["apps"]["http"]["servers"]["http"]["routes"].as_array_mut().unwrap().push(json!({"match":[{"host":["app.localhost"]}],"handle":[{"handler":"static_response","body":"foreign"}]}));
        let mut report = Report::new(false, Duration::from_secs(1));
        inspect_routes(&mut report, &global, &config, &[spec], true, &[]);
        assert!(
            report
                .checks
                .iter()
                .any(|c| c.reason_code == "foreign_hostname"
                    && c.status == Status::Fail
                    && c.remediations.is_empty())
        );
    }

    #[test]
    fn exit_priority_and_expected_skips_are_stable() {
        let mut report = Report::new(false, Duration::ZERO);
        report.simple("ca", Status::Skipped, "not_supplied", "not supplied");
        report.finish();
        assert_eq!(report.exit_code, 0);
        assert!(report.budget("http", Duration::from_secs(1)).is_none());
        report.finish();
        assert_eq!(report.exit_code, 3);
        report.simple("config", Status::Fail, "invalid", "invalid");
        report.finish();
        assert_eq!(report.exit_code, 1);
    }
}
