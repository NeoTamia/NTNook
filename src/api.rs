//! Transport-independent, non-reconciling observation and scoped alias operations.
use crate::{
    caddy, config,
    operation::Budget,
    state::{self, Registry, Store},
};
use clap::{Args, Parser, ValueEnum};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeSet,
    path::PathBuf,
    time::{SystemTime, UNIX_EPOCH},
};
use uuid::Uuid;

type Result<T> = std::result::Result<T, Failure>;

#[derive(Debug, Clone, Copy, Default, ValueEnum, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Scope {
    #[default]
    Context,
    User,
}

#[derive(Debug, Clone, Args, Default)]
pub(crate) struct ContextArgs {
    #[arg(long)]
    pub(crate) cwd: Option<PathBuf>,
    #[arg(long)]
    pub(crate) config: Option<PathBuf>,
    #[arg(long, requires = "config")]
    pub(crate) local: bool,
    #[arg(long, num_args = 0..=1, require_equals = true, default_missing_value = "", conflicts_with = "no_worktree")]
    pub(crate) worktree: Option<String>,
    #[arg(long)]
    pub(crate) no_worktree: bool,
}

#[derive(Debug, Args)]
pub(crate) struct McpArgs {
    #[command(flatten)]
    pub(crate) context: ContextArgs,
    #[arg(long, default_value = "read")]
    pub(crate) tools: String,
    #[arg(long, value_enum, default_value = "context")]
    pub(crate) scope: Scope,
    #[arg(long, value_name = "HOST")]
    pub(crate) allow_alias: Vec<String>,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub(crate) enum Code {
    ConfigInvalid,
    StateInvalid,
    StateVersionUnsupported,
    StateBusy,
    NotFound,
    AmbiguousApp,
    OutOfScope,
    Conflict,
    ForeignRoute,
    OwnershipChanged,
    UnsupportedExposure,
    CaddyUnavailable,
    CaddyConflict,
    PermissionDenied,
    StaleCursor,
    Timeout,
    RecoveryPending,
    Internal,
}
#[derive(Debug, Clone, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Effect {
    None,
    Applied,
    Unknown,
}
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub(crate) struct Failure {
    pub(crate) code: Code,
    pub(crate) message: String,
    pub(crate) retryable: bool,
    pub(crate) operation_id: Option<String>,
    pub(crate) effect: Effect,
    pub(crate) remediation: Vec<String>,
}
impl Failure {
    pub(crate) fn new(code: Code, message: impl ToString) -> Self {
        Self {
            code,
            message: message.to_string(),
            retryable: false,
            operation_id: None,
            effect: Effect::None,
            remediation: vec![],
        }
    }
    fn recovery(mut self, operation: Uuid) -> Self {
        self.operation_id = Some(operation.to_string());
        self.effect = Effect::Unknown;
        self.remediation.push("Inspect nook status --read-only --json; retry this alias operation or run nook prune explicitly.".into());
        self
    }
}
impl From<config::Error> for Failure {
    fn from(error: config::Error) -> Self {
        Self::new(Code::ConfigInvalid, error)
    }
}
impl From<state::Error> for Failure {
    fn from(error: state::Error) -> Self {
        let code = match &error {
            state::Error::UnsupportedVersion(_) => Code::StateVersionUnsupported,
            state::Error::Io { source, .. }
                if source.kind() == std::io::ErrorKind::PermissionDenied =>
            {
                Code::PermissionDenied
            }
            state::Error::Io { source, .. }
                if matches!(
                    source.kind(),
                    std::io::ErrorKind::TimedOut | std::io::ErrorKind::Interrupted
                ) =>
            {
                Code::StateBusy
            }
            _ => Code::StateInvalid,
        };
        Self::new(code, error)
    }
}
impl From<caddy::Error> for Failure {
    fn from(error: caddy::Error) -> Self {
        let code = match &error {
            caddy::Error::ForeignHostname(_) => Code::ForeignRoute,
            caddy::Error::ManagedHostname(_) => Code::Conflict,
            caddy::Error::ConcurrentMutation | caddy::Error::MissingEtag => Code::CaddyConflict,
            _ => Code::CaddyUnavailable,
        };
        Self::new(code, error)
    }
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub(crate) struct Warning {
    pub(crate) code: String,
    pub(crate) message: String,
}
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub(crate) struct ContextInfo {
    scope: Scope,
    worktree_key: Option<String>,
    worktree_label: Option<String>,
}
#[derive(Serialize, JsonSchema)]
#[serde(untagged)]
pub(crate) enum Reply<T> {
    Success {
        schema_version: u32,
        ok: bool,
        observed_at_unix_ms: u64,
        context: ContextInfo,
        data: T,
        warnings: Vec<Warning>,
    },
    Failure {
        schema_version: u32,
        ok: bool,
        observed_at_unix_ms: u64,
        context: ContextInfo,
        error: Failure,
        warnings: Vec<Warning>,
    },
}
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub(crate) struct Worktree {
    key: String,
    label: String,
}
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Kind {
    #[default]
    All,
    Run,
    Alias,
}
#[derive(Debug, Clone, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ResourceKind {
    Run,
    Alias,
}
#[derive(Debug, Clone, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub(crate) enum EntryState {
    Starting,
    Ready,
    Persistent,
}
#[derive(Debug, Clone, Serialize, JsonSchema, PartialEq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Liveness {
    Alive,
    Dead,
    Unknown,
    NotApplicable,
}
#[derive(Debug, Clone, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub(crate) enum UrlKind {
    Local,
    Lan,
    Tailscale,
    Funnel,
}
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub(crate) struct EntryUrl {
    kind: UrlKind,
    url: String,
}
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub(crate) struct Entry {
    pub(crate) resource_id: String,
    pub(crate) kind: ResourceKind,
    pub(crate) hostname: String,
    pub(crate) url: String,
    target: String,
    tls: bool,
    preserve_host: Option<bool>,
    state: EntryState,
    liveness: Liveness,
    pid: Option<u32>,
    group_id: Option<String>,
    worktree: Option<Worktree>,
    urls: Vec<EntryUrl>,
}
#[derive(Serialize, JsonSchema)]
pub(crate) struct ListData {
    entries: Vec<Entry>,
    next_cursor: Option<String>,
}
#[derive(Serialize, JsonSchema)]
pub(crate) struct UrlData {
    hostname: String,
    url: String,
    #[schemars(regex(pattern = "^(registry|configuration)$"))]
    source: String,
    registered: bool,
    entry: Option<Entry>,
}
#[derive(Serialize, JsonSchema)]
pub(crate) struct SetData {
    changed: bool,
    entry: Entry,
    operation_id: Option<String>,
    recovery_pending: bool,
}
#[derive(Serialize, JsonSchema)]
pub(crate) struct RemoveData {
    changed: bool,
    hostname: String,
    removed_owner_id: Option<String>,
    operation_id: Option<String>,
    recovery_pending: bool,
}
#[derive(Serialize, JsonSchema)]
pub(crate) struct ProfilesData {
    contract_version: u32,
    active_profiles: Vec<String>,
    available_profiles: Vec<String>,
    scope: Scope,
    allowed_aliases: Vec<String>,
    tools: Vec<String>,
}
#[derive(Debug, Clone, Serialize, JsonSchema, PartialEq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum CheckState {
    Ok,
    Warning,
    Error,
    Unknown,
    NotApplicable,
}
#[derive(Serialize, JsonSchema)]
pub(crate) struct Check {
    id: String,
    #[schemars(regex(pattern = "^(context|infrastructure)$"))]
    scope: String,
    status: CheckState,
    code: String,
    message: String,
    remediation: Vec<String>,
    affected_resource_ids: Vec<String>,
}
#[derive(Serialize, JsonSchema)]
pub(crate) struct StatusData {
    #[schemars(regex(pattern = "^(ok|degraded|error|unknown)$"))]
    health: String,
    checks: Vec<Check>,
}

#[derive(Debug, Deserialize, JsonSchema, Default)]
#[serde(deny_unknown_fields)]
pub(crate) struct Empty {}
#[derive(Debug, Deserialize, JsonSchema, Default)]
#[serde(deny_unknown_fields)]
pub(crate) struct ListInput {
    #[serde(default)]
    pub(crate) kind: Kind,
    pub(crate) hostname: Option<String>,
    #[schemars(range(min = 1, max = 200))]
    pub(crate) limit: Option<u16>,
    pub(crate) cursor: Option<String>,
    #[schemars(range(min = 100, max = 30000))]
    pub(crate) timeout_ms: Option<u64>,
}
#[derive(Debug, Clone, Copy, Default, Deserialize, JsonSchema, ValueEnum, PartialEq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum UrlMode {
    #[default]
    Registered,
    Configured,
}
#[derive(Debug, Deserialize, JsonSchema, Default)]
#[serde(deny_unknown_fields)]
pub(crate) struct UrlInput {
    pub(crate) hostname: Option<String>,
    pub(crate) app: Option<String>,
    #[serde(default)]
    pub(crate) mode: UrlMode,
    #[schemars(range(min = 100, max = 30000))]
    pub(crate) timeout_ms: Option<u64>,
}
#[derive(Debug, Clone, Copy, Default, Deserialize, JsonSchema, PartialEq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Detail {
    #[default]
    Summary,
    Full,
}
#[derive(Debug, Deserialize, JsonSchema, Default)]
#[serde(deny_unknown_fields)]
pub(crate) struct StatusInput {
    #[serde(default)]
    pub(crate) detail: Detail,
    #[schemars(range(min = 100, max = 30000))]
    pub(crate) timeout_ms: Option<u64>,
}
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(untagged)]
pub(crate) enum Target {
    Port(#[schemars(range(min = 1, max = 65535))] u16),
    Url(String),
}
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct SetInput {
    pub(crate) hostname: String,
    pub(crate) target: Target,
    #[serde(default = "yes")]
    pub(crate) tls: bool,
    #[serde(default)]
    pub(crate) preserve_host: bool,
    #[schemars(range(min = 100, max = 30000))]
    pub(crate) timeout_ms: Option<u64>,
}
fn yes() -> bool {
    true
}
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct RemoveInput {
    pub(crate) hostname: String,
    #[schemars(regex(pattern = "^[0-9a-fA-F-]{36}$"))]
    pub(crate) expected_owner_id: String,
    #[schemars(range(min = 100, max = 30000))]
    pub(crate) timeout_ms: Option<u64>,
}

pub(crate) struct App {
    id: Option<String>,
    hostname: String,
    tls: bool,
}
pub(crate) struct Context {
    pub(crate) global: std::result::Result<config::GlobalConfig, Failure>,
    pub(crate) apps: std::result::Result<Vec<App>, Failure>,
    worktree: Option<crate::worktree::Identity>,
    pub(crate) scope: Scope,
    pub(crate) aliases: bool,
    pub(crate) allowed: BTreeSet<String>,
    state_path: std::result::Result<PathBuf, Failure>,
}
impl Context {
    pub(crate) fn new(
        args: ContextArgs,
        scope: Scope,
        aliases: bool,
        allowed: BTreeSet<String>,
        socket: Option<String>,
    ) -> std::io::Result<Self> {
        let cwd = std::fs::canonicalize(args.cwd.unwrap_or(std::env::current_dir()?))?;
        if !cwd.is_dir() {
            return Err(std::io::Error::other("cwd must be a directory"));
        }
        let global = config::load_global()
            .map(|mut global| {
                if let Some(socket) = socket {
                    global.caddy_admin = format!("unix/{socket}");
                }
                global
            })
            .map_err(Failure::from);
        let mut run = match crate::cli::Cli::try_parse_from(["nook", "run"])
            .expect("run accepts configuration")
            .command
        {
            crate::cli::Command::Run(run) => run,
            _ => unreachable!(),
        };
        run.config = args.config.map(|path| cwd.join(path));
        run.local = args.local;
        run.worktree = args.worktree;
        run.no_worktree = args.no_worktree;
        run.observation = true;
        let mut worktree = None;
        let apps = (|| {
            let bind = global.as_ref().map_err(Clone::clone)?.run_bind_address;
            if let Some(workspace) = crate::workspace_config::load(&run, &cwd, bind)
                .map_err(|error| Failure::new(Code::ConfigInvalid, error))?
            {
                worktree = workspace
                    .apps
                    .first()
                    .and_then(|app| app.config.worktree.clone());
                Ok(workspace
                    .apps
                    .into_iter()
                    .map(|app| App {
                        id: Some(app.id),
                        hostname: app.config.hostname,
                        tls: app.config.tls,
                    })
                    .collect())
            } else {
                let resolved = config::resolve_run(&run, &cwd, bind)?;
                worktree = resolved.worktree;
                Ok(vec![App {
                    id: None,
                    hostname: resolved.hostname,
                    tls: resolved.tls,
                }])
            }
        })();
        Ok(Self {
            global,
            apps,
            worktree,
            scope,
            aliases,
            allowed,
            state_path: state::state_path().map_err(Failure::from),
        })
    }
    fn store(&self, budget: &Budget) -> Result<Store> {
        Ok(
            Store::new(self.state_path.as_ref().map_err(Clone::clone)?.clone())
                .with_budget(budget.clone()),
        )
    }
    fn info(&self) -> ContextInfo {
        ContextInfo {
            scope: self.scope,
            worktree_key: self.worktree.as_ref().map(|w| w.key.clone()),
            worktree_label: self.worktree.as_ref().map(|w| w.label.clone()),
        }
    }
    pub(crate) fn reply<T: Serialize>(&self, result: Result<T>) -> Value {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;
        let warnings = if self.scope == Scope::Context && self.worktree.is_none() {
            vec![Warning { code: "HOSTNAME_SCOPE".into(), message: "Without worktree identity, hostname visibility does not establish project ownership.".into() }]
        } else {
            vec![]
        };
        let reply = match result {
            Ok(data) => Reply::Success {
                schema_version: 1,
                ok: true,
                observed_at_unix_ms: now,
                context: self.info(),
                data,
                warnings,
            },
            Err(error) => Reply::Failure {
                schema_version: 1,
                ok: false,
                observed_at_unix_ms: now,
                context: self.info(),
                error,
                warnings,
            },
        };
        let mut value = serde_json::to_value(reply).expect("API DTOs serialize");
        if matches!(
            value["data"]["entry"]["liveness"].as_str(),
            Some("dead" | "unknown")
        ) {
            value["warnings"].as_array_mut().unwrap().push(json!({"code":"PROCESS_UNAVAILABLE","message":"The URL is registered, but the process is dead or its identity could not be verified."}));
        }
        value
    }
    pub(crate) fn tools(&self) -> Vec<String> {
        let mut tools = vec![
            "nook_tools_profiles",
            "nook_list",
            "nook_url",
            "nook_status",
        ];
        if self.aliases {
            tools.extend(["nook_alias_set", "nook_alias_remove"]);
        }
        tools.into_iter().map(str::to_owned).collect()
    }
    pub(crate) fn profiles(&self) -> ProfilesData {
        ProfilesData {
            contract_version: 1,
            active_profiles: if self.aliases {
                vec!["read".into(), "aliases".into()]
            } else {
                vec!["read".into()]
            },
            available_profiles: vec!["read".into(), "aliases".into()],
            scope: self.scope,
            allowed_aliases: self.allowed.iter().cloned().collect(),
            tools: self.tools(),
        }
    }
    fn visible(&self, entry: &Entry) -> bool {
        if self.scope == Scope::User || self.allowed.contains(&entry.hostname) {
            return true;
        }
        if matches!(entry.kind, ResourceKind::Run)
            && let Some(identity) = &self.worktree
        {
            return entry
                .worktree
                .as_ref()
                .is_some_and(|w| w.key == identity.key);
        }
        self.apps
            .as_ref()
            .is_ok_and(|apps| apps.iter().any(|app| app.hostname == entry.hostname))
    }
    fn entries(&self, registry: &Registry) -> Result<Vec<Entry>> {
        if self.scope == Scope::Context {
            self.apps.as_ref().map_err(Clone::clone)?;
        }
        let mut entries: Vec<_> = registry
            .leases
            .values()
            .map(|lease| {
                let live = match crate::process::lease_liveness(lease) {
                    crate::process::Liveness::Alive => Liveness::Alive,
                    crate::process::Liveness::Dead => Liveness::Dead,
                    crate::process::Liveness::Indeterminate => Liveness::Unknown,
                };
                let mut entry = base_entry(lease.id, &lease.hostname, &lease.target, lease.tls);
                entry.kind = ResourceKind::Run;
                entry.preserve_host = None;
                entry.state = match lease.state {
                    state::LeaseState::Starting => EntryState::Starting,
                    state::LeaseState::Ready => EntryState::Ready,
                };
                entry.liveness = live;
                entry.pid = Some(lease.pid);
                entry.group_id = lease.group_id.map(|id| id.to_string());
                entry.worktree = lease.worktree.as_ref().map(|w| Worktree {
                    key: w.key.clone(),
                    label: w.label.clone(),
                });
                if lease.lan {
                    entry.urls.push(EntryUrl {
                        kind: UrlKind::Lan,
                        url: format!("http://{}", crate::lan::hostname(&lease.hostname)),
                    });
                }
                entry
            })
            .chain(registry.aliases.values().map(alias_entry))
            .filter(|entry| self.visible(entry))
            .collect();
        for entry in &mut entries {
            if let Some(dns) = &registry.tailscale.dns_name {
                for (port, registration) in &registry.tailscale.registrations {
                    if registration.owner_id.to_string() == entry.resource_id {
                        entry.urls.push(EntryUrl {
                            kind: if registration.funnel {
                                UrlKind::Funnel
                            } else {
                                UrlKind::Tailscale
                            },
                            url: if *port == 443 {
                                format!("https://{dns}")
                            } else {
                                format!("https://{dns}:{port}")
                            },
                        });
                    }
                }
            }
        }
        entries.sort_by(|a, b| {
            (
                &a.hostname,
                matches!(a.kind, ResourceKind::Run),
                &a.resource_id,
            )
                .cmp(&(
                    &b.hostname,
                    matches!(b.kind, ResourceKind::Run),
                    &b.resource_id,
                ))
        });
        Ok(entries)
    }
    pub(crate) fn list(&self, input: ListInput, budget: &Budget) -> Result<ListData> {
        let registry = self.store(budget)?.snapshot()?;
        let hostname = input
            .hostname
            .map(|host| full_hostname(&host))
            .transpose()?;
        let entries: Vec<_> = self
            .entries(&registry)?
            .into_iter()
            .filter(|e| {
                (input.kind == Kind::All
                    || matches!(
                        (&input.kind, &e.kind),
                        (Kind::Run, ResourceKind::Run) | (Kind::Alias, ResourceKind::Alias)
                    ))
                    && hostname.as_ref().is_none_or(|h| h == &e.hostname)
            })
            .collect();
        let digest =
            Sha256::digest(serde_json::to_vec(&json!([entries, input.kind, hostname])).unwrap())
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>();
        let start = match input.cursor {
            None => 0,
            Some(cursor) => {
                let (hash, offset) = cursor
                    .split_once(':')
                    .ok_or_else(|| Failure::new(Code::StaleCursor, "Invalid cursor"))?;
                let offset = offset
                    .parse::<usize>()
                    .map_err(|_| Failure::new(Code::StaleCursor, "Invalid cursor"))?;
                if hash != digest || offset > entries.len() {
                    return Err(Failure::new(
                        Code::StaleCursor,
                        "Snapshot changed; restart pagination",
                    ));
                }
                offset
            }
        };
        let end = (start + input.limit.unwrap_or(100) as usize).min(entries.len());
        Ok(ListData {
            next_cursor: (end < entries.len()).then(|| format!("{digest}:{end}")),
            entries: entries[start..end].to_vec(),
        })
    }
    pub(crate) fn url(&self, input: UrlInput, budget: &Budget) -> Result<UrlData> {
        let (hostname, planned_tls) = if let Some(host) = input.hostname {
            (full_hostname(&host)?, None)
        } else {
            let apps = self.apps.as_ref().map_err(Clone::clone)?;
            let candidates: Vec<_> = apps
                .iter()
                .filter(|app| {
                    input
                        .app
                        .as_ref()
                        .is_none_or(|id| app.id.as_ref() == Some(id))
                })
                .collect();
            if candidates.is_empty() {
                return Err(Failure::new(
                    Code::NotFound,
                    "Application is not configured",
                ));
            }
            if candidates.len() != 1 {
                return Err(Failure::new(
                    Code::AmbiguousApp,
                    "Select a workspace application using app",
                ));
            }
            (candidates[0].hostname.clone(), Some(candidates[0].tls))
        };
        let registry = self.store(budget)?.snapshot()?;
        let matches: Vec<_> = self
            .entries(&registry)?
            .into_iter()
            .filter(|entry| entry.hostname == hostname)
            .collect();
        if matches.len() > 1 {
            return Err(Failure::new(Code::Conflict, "Multiple registered owners"));
        }
        let entry = matches.into_iter().next();
        if input.mode == UrlMode::Configured {
            return Ok(UrlData {
                url: local_url(
                    &hostname,
                    planned_tls.expect("validated configured selector"),
                ),
                hostname,
                source: "configuration".into(),
                registered: entry.is_some(),
                entry,
            });
        }
        let entry = entry
            .ok_or_else(|| Failure::new(Code::NotFound, "No registered resource in this scope"))?;
        Ok(UrlData {
            hostname,
            url: entry.url.clone(),
            source: "registry".into(),
            registered: true,
            entry: Some(entry),
        })
    }
}
fn local_url(host: &str, tls: bool) -> String {
    format!("{}://{host}", if tls { "https" } else { "http" })
}
pub(crate) fn full_hostname(host: &str) -> Result<String> {
    if !host.to_ascii_lowercase().ends_with(".localhost") {
        return Err(Failure::new(
            Code::ConfigInvalid,
            "A complete .localhost hostname is required",
        ));
    }
    config::normalize_hostname(host).map_err(Into::into)
}
fn base_entry(id: Uuid, hostname: &str, target: &str, tls: bool) -> Entry {
    let url = local_url(hostname, tls);
    Entry {
        resource_id: id.to_string(),
        kind: ResourceKind::Alias,
        hostname: hostname.into(),
        url: url.clone(),
        target: target.into(),
        tls,
        preserve_host: Some(false),
        state: EntryState::Persistent,
        liveness: Liveness::NotApplicable,
        pid: None,
        group_id: None,
        worktree: None,
        urls: vec![EntryUrl {
            kind: UrlKind::Local,
            url,
        }],
    }
}
fn alias_entry(alias: &state::Alias) -> Entry {
    let mut e = base_entry(alias.id, &alias.hostname, &alias.target, alias.tls);
    e.preserve_host = Some(alias.preserve_host);
    e
}

fn check(id: &str, scope: &str, status: CheckState, code: &str, message: impl ToString) -> Check {
    Check {
        id: id.into(),
        scope: scope.into(),
        status,
        code: code.into(),
        message: message.to_string(),
        remediation: vec![],
        affected_resource_ids: vec![],
    }
}
impl Context {
    pub(crate) fn status(&self, input: StatusInput, budget: &Budget) -> Result<StatusData> {
        let mut checks = vec![];
        match &self.apps {
            Ok(_) => checks.push(check(
                "config",
                "context",
                CheckState::Ok,
                "CONFIG_VALID",
                "Configuration resolved; restart MCP after changing configuration",
            )),
            Err(error) => checks.push(check(
                "config",
                "context",
                CheckState::Error,
                "CONFIG_INVALID",
                &error.message,
            )),
        }
        let registry = self
            .store(budget)
            .and_then(|store| store.snapshot().map_err(Into::into));
        let entries = registry.as_ref().ok().and_then(|r| self.entries(r).ok());
        match &registry {
            Ok(registry) => {
                checks.push(check(
                    "registry",
                    "infrastructure",
                    CheckState::Ok,
                    "REGISTRY_READABLE",
                    "Registry snapshot read without reconciliation",
                ));
                let ids: BTreeSet<_> = entries
                    .as_ref()
                    .into_iter()
                    .flatten()
                    .map(|e| e.resource_id.as_str())
                    .collect();
                let count = registry
                    .pending_operations
                    .iter()
                    .filter(|op| {
                        self.scope == Scope::User
                            || pending_host(&op.kind).is_some_and(|h| {
                                self.allowed.contains(h)
                                    || self
                                        .apps
                                        .as_ref()
                                        .is_ok_and(|apps| apps.iter().any(|app| app.hostname == h))
                            })
                            || pending_owner(&op.kind)
                                .is_some_and(|id| ids.contains(id.to_string().as_str()))
                    })
                    .count();
                let mut pending = check(
                    "pending_operations",
                    "context",
                    if count == 0 {
                        CheckState::Ok
                    } else {
                        CheckState::Warning
                    },
                    "PENDING_OPERATIONS",
                    format!("{count} pending operations; no recovery was performed"),
                );
                if count > 0 {
                    pending.remediation.push("Inspect ownership, then run nook prune explicitly if global recovery is intended.".into());
                }
                checks.push(pending);
            }
            Err(error) => {
                checks.push(check(
                    "registry",
                    "infrastructure",
                    CheckState::Error,
                    "REGISTRY_UNREADABLE",
                    &error.message,
                ));
                checks.push(check(
                    "pending_operations",
                    "context",
                    CheckState::Unknown,
                    "REGISTRY_UNREADABLE",
                    "Cannot inspect pending operations",
                ));
            }
        }
        let mut live = check(
            "process_liveness",
            "context",
            CheckState::Unknown,
            "LIVENESS_UNKNOWN",
            "Cannot inspect scoped leases",
        );
        if let Some(entries) = &entries {
            live.status = if entries.iter().any(|e| e.liveness == Liveness::Dead) {
                CheckState::Warning
            } else if entries.iter().any(|e| e.liveness == Liveness::Unknown) {
                CheckState::Unknown
            } else {
                CheckState::Ok
            };
            live.message =
                "Process identity observed; recorded readiness is not a health probe".into();
            live.affected_resource_ids = entries
                .iter()
                .filter(|e| matches!(e.liveness, Liveness::Dead | Liveness::Unknown))
                .map(|e| e.resource_id.clone())
                .collect();
        }
        checks.push(live);
        let caddy = (|| {
            let global = self.global.as_ref().map_err(Clone::clone)?;
            let client = caddy::Client::new(&global.caddy_admin)?.with_budget(budget.clone());
            let config = client.fetch_config()?;
            Ok::<_, Failure>((client, config))
        })();
        match caddy {
            Err(error) => {
                checks.push(check(
                    "caddy_admin",
                    "infrastructure",
                    CheckState::Error,
                    "CADDY_UNAVAILABLE",
                    error.message,
                ));
                for id in ["servers", "managed_routes"] {
                    checks.push(check(
                        id,
                        "infrastructure",
                        CheckState::Unknown,
                        "CADDY_UNAVAILABLE",
                        "Caddy could not be inspected",
                    ));
                }
                if input.detail == Detail::Full {
                    checks.push(check(
                        "local_ca",
                        "infrastructure",
                        CheckState::Unknown,
                        "CADDY_UNAVAILABLE",
                        "CA could not be inspected",
                    ));
                }
            }
            Ok((client, config)) => {
                checks.push(check(
                    "caddy_admin",
                    "infrastructure",
                    CheckState::Ok,
                    "CADDY_REACHABLE",
                    "Admin API responded",
                ));
                let global = self.global.as_ref().map_err(Clone::clone)?;
                let selection = caddy::discover_available_servers(
                    &config,
                    caddy::ServerOverrides {
                        https: global.https_server.as_deref(),
                        http: global.http_server.as_deref(),
                    },
                );
                match selection {
                    Err(error) => {
                        checks.push(check(
                            "servers",
                            "infrastructure",
                            CheckState::Error,
                            "SERVERS_INVALID",
                            error,
                        ));
                        checks.push(check(
                            "managed_routes",
                            "context",
                            CheckState::Unknown,
                            "SERVERS_INVALID",
                            "No unambiguous server selection",
                        ));
                    }
                    Ok(selection) => {
                        let needs_https = self
                            .apps
                            .as_ref()
                            .is_ok_and(|apps| apps.iter().any(|a| a.tls))
                            || entries.as_ref().is_some_and(|e| e.iter().any(|e| e.tls));
                        let needs_http = self
                            .apps
                            .as_ref()
                            .is_ok_and(|apps| apps.iter().any(|a| !a.tls))
                            || entries.as_ref().is_some_and(|e| {
                                e.iter().any(|e| {
                                    !e.tls || e.urls.iter().any(|u| matches!(u.kind, UrlKind::Lan))
                                })
                            });
                        checks.push(check(
                            "servers",
                            "infrastructure",
                            if needs_https && selection.https.is_none()
                                || needs_http && selection.http.is_none()
                            {
                                CheckState::Error
                            } else {
                                CheckState::Ok
                            },
                            "SERVER_SELECTION",
                            format!("HTTPS: {:?}; HTTP: {:?}", selection.https, selection.http),
                        ));
                        match (caddy::inspect_managed(&config, &selection), &entries) {
                            (Ok(inspection), Some(entries)) => {
                                let mut drift = check(
                                    "managed_routes",
                                    "context",
                                    CheckState::Ok,
                                    "ROUTE_DRIFT",
                                    "Ownership and route presence inspected without repair",
                                );
                                drift.affected_resource_ids = entries
                                    .iter()
                                    .filter(|entry| {
                                        !inspection.routes.iter().any(|route| {
                                            route.owner_id.to_string() == entry.resource_id
                                                && route.hostname == entry.hostname
                                                && route.tls == entry.tls
                                        })
                                    })
                                    .map(|e| e.resource_id.clone())
                                    .collect();
                                let orphan = inspection.routes.iter().any(|route| {
                                    registry.as_ref().is_ok_and(|registry| {
                                        !registry.leases.contains_key(&route.owner_id)
                                            && !registry
                                                .aliases
                                                .values()
                                                .any(|alias| alias.id == route.owner_id)
                                    }) && (self.scope == Scope::User
                                        || self.allowed.contains(&route.hostname)
                                        || self.apps.as_ref().is_ok_and(|apps| {
                                            apps.iter().any(|a| a.hostname == route.hostname)
                                        }))
                                });
                                if !drift.affected_resource_ids.is_empty() || orphan {
                                    drift.status = CheckState::Warning;
                                    drift.remediation.push("Inspect drift; use nook prune explicitly to reconcile owned resources.".into());
                                }
                                checks.push(drift);
                            }
                            (Err(error), _) => checks.push(check(
                                "managed_routes",
                                "context",
                                CheckState::Error,
                                "ROUTES_INVALID",
                                error,
                            )),
                            _ => checks.push(check(
                                "managed_routes",
                                "context",
                                CheckState::Unknown,
                                "REGISTRY_UNREADABLE",
                                "Cannot compare registry and Caddy",
                            )),
                        }
                    }
                }
                if input.detail == Detail::Full {
                    let needs_tls = self
                        .apps
                        .as_ref()
                        .is_ok_and(|apps| apps.iter().any(|app| app.tls))
                        || entries
                            .as_ref()
                            .is_some_and(|entries| entries.iter().any(|e| e.tls));
                    let ca = if !needs_tls && self.apps.is_ok() && entries.is_some() {
                        check(
                            "local_ca",
                            "infrastructure",
                            CheckState::NotApplicable,
                            "HTTPS_NOT_REQUIRED",
                            "No scoped HTTPS requirement",
                        )
                    } else {
                        match client
                            .fetch_local_ca()
                            .and_then(|pem| caddy::local_ca_is_trusted(&pem))
                        {
                            Ok(trusted) => {
                                let mut c = check(
                                    "local_ca",
                                    "infrastructure",
                                    if trusted {
                                        CheckState::Ok
                                    } else {
                                        CheckState::Warning
                                    },
                                    "CA_TRUST",
                                    if trusted {
                                        "Local CA trusted"
                                    } else {
                                        "Local CA not trusted"
                                    },
                                );
                                if !trusted {
                                    c.remediation.push(client.trust_command());
                                }
                                c
                            }
                            Err(error) => check(
                                "local_ca",
                                "infrastructure",
                                CheckState::Unknown,
                                "CA_UNKNOWN",
                                error,
                            ),
                        }
                    };
                    checks.push(ca);
                }
            }
        }
        let health = if checks.iter().any(|c| c.status == CheckState::Error) {
            "error"
        } else if checks.iter().any(|c| c.status == CheckState::Warning) {
            "degraded"
        } else if checks.iter().any(|c| c.status == CheckState::Unknown) {
            "unknown"
        } else {
            "ok"
        };
        Ok(StatusData {
            health: health.into(),
            checks,
        })
    }

    fn authorized(&self, hostname: &str) -> Result<String> {
        let hostname = full_hostname(hostname)?;
        if !self.aliases || !self.allowed.contains(&hostname) {
            return Err(Failure::new(
                Code::OutOfScope,
                "Alias hostname is not authorized by --allow-alias",
            ));
        }
        Ok(hostname)
    }
    fn alias_client(&self, budget: &Budget) -> Result<caddy::Client> {
        Ok(
            caddy::Client::new(&self.global.as_ref().map_err(Clone::clone)?.caddy_admin)?
                .with_budget(budget.clone()),
        )
    }
    fn alias_routes<'a>(
        &'a self,
        client: &'a caddy::Client,
        selection: &'a caddy::ServerSelection,
    ) -> Result<caddy::ManagedCaddyRoutes<'a>> {
        let g = self.global.as_ref().map_err(Clone::clone)?;
        Ok(caddy::ManagedCaddyRoutes {
            client,
            https_server: selection.https.as_deref(),
            http_server: selection.http.as_deref(),
            loopback_host: &g.caddy_loopback_host,
            client_ip_ranges: &g.caddy_client_ip_ranges,
        })
    }
    fn selection(&self, config: &Value, tls: bool) -> Result<caddy::ServerSelection> {
        let g = self.global.as_ref().map_err(Clone::clone)?;
        caddy::discover_servers(
            config,
            caddy::ServerOverrides {
                https: g.https_server.as_deref(),
                http: g.http_server.as_deref(),
            },
            tls,
            !tls,
        )
        .map_err(Into::into)
    }
    pub(crate) fn set_alias(&self, input: SetInput, budget: &Budget) -> Result<SetData> {
        let hostname = self.authorized(&input.hostname)?;
        let target = match input.target {
            Target::Port(port) => port.to_string(),
            Target::Url(url) => url,
        };
        let upstream = caddy::normalize_upstream(&target)
            .map_err(|error| Failure::new(Code::ConfigInvalid, error))?;
        let store = self.store(budget)?;
        let _guard = store.lock_operations()?;
        let registry = store.snapshot()?;
        if registry
            .leases
            .values()
            .any(|lease| lease.hostname == hostname)
        {
            return Err(Failure::new(
                Code::Conflict,
                "A run owns this hostname; MCP cannot transfer ownership",
            ));
        }
        let previous = registry.aliases.get(&hostname);
        if registry
            .tailscale
            .registrations
            .values()
            .any(|r| r.hostname == hostname || previous.is_some_and(|alias| alias.id == r.owner_id))
        {
            return Err(Failure::new(
                Code::UnsupportedExposure,
                "Use the CLI to manage an alias with a Tailscale registration",
            ));
        }
        if previous.is_some_and(|a| {
            a.target != upstream.url.as_str()
                || a.tls != input.tls
                || a.preserve_host != input.preserve_host
        }) {
            return Err(Failure::new(
                Code::Conflict,
                "Alias differs; explicitly remove its observed owner before creating another",
            ));
        }
        let alias = previous.cloned().unwrap_or_else(|| state::Alias {
            id: Uuid::new_v4(),
            hostname: hostname.clone(),
            target: upstream.url.to_string(),
            scheme: if upstream.url.scheme() == "https" {
                state::Scheme::Https
            } else {
                state::Scheme::Http
            },
            tls: input.tls,
            preserve_host: input.preserve_host,
        });
        if registry.pending_operations.iter().any(|op| {
            pending_host(&op.kind) == Some(hostname.as_str())
                && (pending_owner(&op.kind) != Some(alias.id)
                    || matches!(op.kind, state::PendingOperationKind::RemoveRoute { .. }))
        }) {
            return Err(Failure::new(
                Code::RecoveryPending,
                "A different operation owns pending recovery for this hostname; inspect status before pruning",
            ));
        }
        let client = self.alias_client(budget)?;
        let config = client.fetch_config()?;
        let selection = self.selection(&config, alias.tls)?;
        let routes = self.alias_routes(&client, &selection)?;
        routes.check_alias(&config, &alias)?;
        let matching = routes.alias_matches(&config, &alias)?;
        let pending = registry
            .pending_operations
            .iter()
            .find(|op| pending_owner(&op.kind) == Some(alias.id))
            .map(|op| op.id);
        if previous.is_some() && matching && pending.is_none() {
            return Ok(SetData {
                changed: false,
                entry: alias_entry(&alias),
                operation_id: None,
                recovery_pending: false,
            });
        }
        let operation_id = pending.unwrap_or_else(Uuid::new_v4);
        store.mutate(|registry| {
            registry.aliases.insert(hostname.clone(), alias.clone());
            if pending.is_none() {
                registry.pending_operations.push(state::PendingOperation {
                    id: operation_id,
                    kind: state::PendingOperationKind::RestoreRoute {
                        hostname: hostname.clone(),
                        target: alias.target.clone(),
                        scheme: alias.scheme,
                        owner_id: alias.id,
                        tls: alias.tls,
                    },
                });
            }
            Ok(())
        })?;
        if !matching {
            routes
                .ensure_alias(&crate::reconcile::RouteSpec {
                    lan: false,
                    owner_id: alias.id,
                    hostname,
                    target: alias.target.clone(),
                    scheme: alias.scheme,
                    tls: alias.tls,
                    replace_existing: false,
                    preserve_host: alias.preserve_host,
                })
                .map_err(|e| Failure::from(e).recovery(operation_id))?;
        }
        store
            .mutate(|r| {
                r.pending_operations.retain(|op| op.id != operation_id);
                Ok(())
            })
            .map_err(|e| {
                let mut error = Failure::from(e).recovery(operation_id);
                error.effect = Effect::Applied;
                error
            })?;
        Ok(SetData {
            changed: true,
            entry: alias_entry(&alias),
            operation_id: Some(operation_id.to_string()),
            recovery_pending: false,
        })
    }
    pub(crate) fn remove_alias(&self, input: RemoveInput, budget: &Budget) -> Result<RemoveData> {
        let hostname = self.authorized(&input.hostname)?;
        let owner = Uuid::parse_str(&input.expected_owner_id)
            .map_err(|_| Failure::new(Code::ConfigInvalid, "Invalid owner UUID"))?;
        let store = self.store(budget)?;
        let _guard = store.lock_operations()?;
        let registry = store.snapshot()?;
        let alias = registry.aliases.get(&hostname);
        if alias.is_some_and(|a| a.id != owner)
            || registry.leases.values().any(|l| l.hostname == hostname)
        {
            return Err(Failure::new(
                Code::OwnershipChanged,
                "The hostname no longer belongs to the expected alias",
            ));
        }
        if registry
            .tailscale
            .registrations
            .values()
            .any(|r| r.hostname == hostname || r.owner_id == owner)
        {
            return Err(Failure::new(
                Code::UnsupportedExposure,
                "Use the CLI to remove an alias with a Tailscale registration",
            ));
        }
        let pending = registry
            .pending_operations
            .iter()
            .find_map(|op| match &op.kind {
                state::PendingOperationKind::RemoveRoute {
                    hostname: h,
                    owner_id,
                    tls,
                } if h == &hostname && *owner_id == owner => Some((op.id, *tls)),
                _ => None,
            });
        if alias.is_none() && pending.is_none() {
            return Ok(RemoveData {
                changed: false,
                hostname,
                removed_owner_id: None,
                operation_id: None,
                recovery_pending: false,
            });
        }
        let tls = alias.map(|a| a.tls).unwrap_or_else(|| pending.unwrap().1);
        let client = self.alias_client(budget)?;
        let config = client.fetch_config()?;
        let selection = self.selection(&config, tls)?;
        let routes = self.alias_routes(&client, &selection)?;
        let operation_id = pending.map(|p| p.0).unwrap_or_else(Uuid::new_v4);
        store.mutate(|r| {
            r.aliases.remove(&hostname);
            r.pending_operations
                .retain(|op| pending_owner(&op.kind) != Some(owner));
            r.pending_operations.push(state::PendingOperation {
                id: operation_id,
                kind: state::PendingOperationKind::RemoveRoute {
                    hostname: hostname.clone(),
                    owner_id: owner,
                    tls,
                },
            });
            Ok(())
        })?;
        routes
            .remove_alias(owner, tls)
            .map_err(|e| Failure::from(e).recovery(operation_id))?;
        store
            .mutate(|r| {
                r.pending_operations.retain(|op| op.id != operation_id);
                Ok(())
            })
            .map_err(|e| {
                let mut error = Failure::from(e).recovery(operation_id);
                error.effect = Effect::Applied;
                error
            })?;
        Ok(RemoveData {
            changed: true,
            hostname,
            removed_owner_id: Some(owner.to_string()),
            operation_id: Some(operation_id.to_string()),
            recovery_pending: false,
        })
    }
}
fn pending_owner(kind: &state::PendingOperationKind) -> Option<Uuid> {
    use state::PendingOperationKind::*;
    Some(match kind {
        InstallRoute { owner_id, .. }
        | RestoreRoute { owner_id, .. }
        | RemoveRoute { owner_id, .. }
        | StartProcess { owner_id, .. } => *owner_id,
        FinalizeLease { lease_id } => *lease_id,
    })
}
fn pending_host(kind: &state::PendingOperationKind) -> Option<&str> {
    use state::PendingOperationKind::*;
    match kind {
        InstallRoute { hostname, .. }
        | RestoreRoute { hostname, .. }
        | RemoveRoute { hostname, .. }
        | StartProcess { hostname, .. } => Some(hostname),
        FinalizeLease { .. } => None,
    }
}
