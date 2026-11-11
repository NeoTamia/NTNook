//! CLI-only Compose orchestration. The durable project owns routes, not the CLI PID.
use crate::{
    compose_config::{self, Endpoint, IDENTITY, OWNER},
    reconcile::RouteBackend,
    state::{Registry, Store},
};
use clap::{Args as ClapArgs, Subcommand};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::OsString,
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};
use uuid::Uuid;

#[derive(Debug, ClapArgs)]
pub(crate) struct Args {
    #[arg(short = 'f', long = "file")]
    files: Vec<PathBuf>,
    #[arg(short = 'p', long = "project-name")]
    project: Option<String>,
    #[arg(long)]
    profile: Vec<String>,
    #[arg(long)]
    env_file: Vec<PathBuf>,
    #[arg(long)]
    project_directory: Option<PathBuf>,
    #[arg(long, num_args=0..=1, require_equals=true, default_missing_value="", conflicts_with="no_worktree")]
    worktree: Option<String>,
    #[arg(long)]
    no_worktree: bool,
    #[command(subcommand)]
    command: Action,
}

#[derive(Debug, Subcommand)]
enum Action {
    /// Start an application and publish explicitly labelled HTTP endpoints.
    Up(Up),
    /// Remove only owned containers and routes; always retain volumes and images.
    Down(Down),
    /// Show Compose containers with the effective project identity.
    Ps {
        #[arg(long)]
        all: bool,
    },
    /// Read application logs (Ctrl+C never stops containers).
    Logs {
        #[arg(short, long)]
        follow: bool,
        #[arg(long)]
        tail: Option<u32>,
        services: Vec<String>,
    },
    /// Print resolved Compose configuration (may contain secrets).
    Config,
}

#[derive(Debug, ClapArgs)]
struct Up {
    #[arg(short, long)]
    detach: bool,
    #[arg(long, conflicts_with = "no_build")]
    build: bool,
    #[arg(long)]
    no_build: bool,
    #[arg(long, value_parser=["always", "missing", "never"])]
    pull: Option<String>,
    #[arg(long, conflicts_with = "no_recreate")]
    force_recreate: bool,
    #[arg(long)]
    no_recreate: bool,
    #[arg(long)]
    no_deps: bool,
    #[arg(long)]
    wait: bool,
    #[arg(long, requires = "wait", default_value = "60", value_parser=clap::value_parser!(u64).range(1..))]
    wait_timeout: u64,
    services: Vec<String>,
}

#[derive(Debug, ClapArgs)]
struct Down {
    #[arg(long, default_value = "10")]
    timeout: u32,
    services: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Project {
    id: Uuid,
    identity: String,
    pub(crate) root: PathBuf,
    pub(crate) name: String,
    namespace: String,
    host: String,
    engine: String,
    pub(crate) routes: BTreeMap<String, Endpoint>,
    services: BTreeSet<String>,
}

pub(crate) fn invalid(message: impl Into<String>) -> io::Error {
    io::Error::other(message.into())
}

/// Recorded intent only: observation must not discover Docker or reconcile routes.
pub(crate) fn observations(registry: &Registry) -> impl Iterator<Item = (&Project, &Endpoint)> {
    registry.compose_projects.values().flat_map(|project| {
        project
            .routes
            .values()
            .map(move |endpoint| (project, endpoint))
    })
}
fn hash(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

#[derive(Clone)]
struct Docker {
    host: String,
}
impl Docker {
    fn current() -> crate::Result<Self> {
        let host = if std::env::var_os("DOCKER_CONTEXT").is_none() {
            std::env::var("DOCKER_HOST").ok().filter(|h| !h.is_empty())
        } else {
            None
        };
        let host = match host {
            Some(host) => host,
            None => {
                let contexts = capture(Command::new("docker").args(["context", "inspect"]))?;
                let value: Value = serde_json::from_slice(&contexts).map_err(invalid_json)?;
                value[0]["Endpoints"]["docker"]["Host"]
                    .as_str()
                    .ok_or_else(|| invalid("cannot resolve Docker context endpoint"))?
                    .to_owned()
            }
        };
        if !(host.starts_with("unix://") || host.starts_with("npipe://")) {
            return Err(invalid(
                "Compose V1 requires a local Docker engine (unix or npipe endpoint)",
            )
            .into());
        }
        Ok(Self { host })
    }
    fn command(&self) -> Command {
        let mut cmd = Command::new("docker");
        cmd.env_remove("DOCKER_CONTEXT")
            .env("DOCKER_HOST", &self.host);
        cmd
    }
    fn json(&self, args: &[&str]) -> crate::Result<Value> {
        serde_json::from_slice(&capture(self.command().args(args))?)
            .map_err(|e| invalid_json(e).into())
    }
    fn engine(&self) -> crate::Result<String> {
        let info = self.json(&["info", "--format", "{{json .}}"])?;
        if info["OSType"] != "linux" {
            return Err(invalid("Compose V1 supports Linux containers only").into());
        }
        Ok(info["ID"]
            .as_str()
            .filter(|s| !s.is_empty())
            .ok_or_else(|| invalid("Docker engine returned no identity"))?
            .into())
    }
    fn containers(&self, project: &Project) -> crate::Result<Vec<Value>> {
        let items = self.observe_containers(project)?;
        for item in &items {
            check_container(item, project)?;
        }
        Ok(items)
    }
    fn observe_containers(&self, project: &Project) -> crate::Result<Vec<Value>> {
        let filter = format!("label=com.docker.compose.project={}", project.name);
        let ids = capture(self.command().args(["ps", "-aq", "--filter", &filter]))?;
        let text = String::from_utf8(ids).map_err(|_| invalid("invalid Docker IDs"))?;
        let ids: Vec<_> = text.split_whitespace().collect();
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        let mut args = vec!["inspect"];
        args.extend(ids);
        let value = self.json(&args)?;
        let items = value
            .as_array()
            .ok_or_else(|| invalid("invalid Docker inspect response"))?
            .clone();
        Ok(items)
    }
}
fn invalid_json(_: serde_json::Error) -> io::Error {
    invalid("Docker returned invalid JSON (output suppressed because it may contain secrets)")
}

/// Bound discovery calls; drain both pipes concurrently to avoid deadlocks on large models.
fn capture(command: &mut Command) -> crate::Result<Vec<u8>> {
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let mut stdout = child.stdout.take().unwrap();
    let mut stderr = child.stderr.take().unwrap();
    let out = thread::spawn(move || {
        let mut b = Vec::new();
        stdout.read_to_end(&mut b).map(|_| b)
    });
    let err = thread::spawn(move || {
        let mut b = Vec::new();
        stderr.read_to_end(&mut b).map(|_| b)
    });
    let deadline = Instant::now() + Duration::from_secs(20);
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(invalid("Docker discovery timed out after 20 seconds").into());
        }
        thread::sleep(Duration::from_millis(20));
    };
    let bytes = out
        .join()
        .map_err(|_| invalid("Docker output reader failed"))??;
    let _ = err.join();
    if !status.success() {
        return Err(invalid(format!("Docker discovery/configuration failed ({status}); run the corresponding docker command to inspect its diagnostics")).into());
    }
    Ok(bytes)
}

fn check_container(container: &Value, project: &Project) -> crate::Result<()> {
    let labels = &container["Config"]["Labels"];
    if labels[OWNER].as_str() != Some(project.id.to_string().as_str())
        || labels[IDENTITY].as_str() != Some(&project.identity)
        || labels["com.docker.compose.project"].as_str() != Some(&project.name)
        || labels["com.docker.compose.service"].as_str().is_none()
    {
        return Err(invalid(format!(
            "project {} contains a foreign container; refusing adoption or mutation",
            project.name
        ))
        .into());
    }
    Ok(())
}

struct Invocation {
    root: PathBuf,
    cwd: PathBuf,
    args: Vec<OsString>,
}
impl Invocation {
    fn resolve(args: &Args) -> crate::Result<Self> {
        let cwd = std::env::current_dir()?;
        let mut files = args.files.clone();
        if files.is_empty()
            && let Some(value) = std::env::var_os("COMPOSE_FILE")
        {
            let sep = std::env::var("COMPOSE_PATH_SEPARATOR")
                .unwrap_or_else(|_| if cfg!(windows) { ";" } else { ":" }.into());
            files = value
                .to_string_lossy()
                .split(&sep)
                .map(PathBuf::from)
                .collect();
        }
        if files.is_empty() {
            let start = args.project_directory.as_deref().unwrap_or(&cwd);
            for dir in start.ancestors() {
                if let Some(base) = [
                    "compose.yaml",
                    "compose.yml",
                    "docker-compose.yml",
                    "docker-compose.yaml",
                ]
                .iter()
                .map(|f| dir.join(f))
                .find(|f| f.is_file())
                {
                    files.push(base);
                    if let Some(over) = [
                        "compose.override.yaml",
                        "compose.override.yml",
                        "docker-compose.override.yml",
                        "docker-compose.override.yaml",
                    ]
                    .iter()
                    .map(|f| dir.join(f))
                    .find(|f| f.is_file())
                    {
                        files.push(over);
                    }
                    break;
                }
            }
        }
        if files.is_empty() {
            return Err(invalid("no Compose file found; use -f").into());
        }
        for file in &mut files {
            *file = fs::canonicalize(cwd.join(&*file))?;
        }
        let root = fs::canonicalize(
            args.project_directory
                .as_ref()
                .map(|p| cwd.join(p))
                .unwrap_or_else(|| files[0].parent().unwrap().into()),
        )?;
        let mut command_args = vec![
            OsString::from("compose"),
            "--project-directory".into(),
            root.clone().into_os_string(),
        ];
        for file in &files {
            command_args.extend(["-f".into(), file.clone().into_os_string()]);
        }
        for file in &args.env_file {
            command_args.extend([
                "--env-file".into(),
                fs::canonicalize(cwd.join(file))?.into_os_string(),
            ]);
        }
        let profiles = if args.profile.is_empty() {
            std::env::var("COMPOSE_PROFILES")
                .unwrap_or_default()
                .split(',')
                .filter(|s| !s.is_empty())
                .map(str::to_owned)
                .collect()
        } else {
            args.profile.clone()
        };
        for profile in &profiles {
            command_args.extend(["--profile".into(), profile.into()]);
        }
        Ok(Self {
            root,
            cwd,
            args: command_args,
        })
    }
    fn command(&self, docker: &Docker, name: Option<&str>) -> Command {
        let mut command = docker.command();
        command.current_dir(&self.cwd).args(&self.args);
        if let Some(name) = name {
            command.args(["-p", name]);
        }
        command.env("COMPOSE_MENU", "false");
        command
    }
    fn model(
        &self,
        docker: &Docker,
        name: Option<&str>,
        override_path: Option<&Path>,
    ) -> crate::Result<Value> {
        let mut cmd = self.command(docker, name);
        if let Some(path) = override_path {
            cmd.arg("-f").arg(path);
        }
        // All profiles are included in validation, but only selected services are launched.
        cmd.args(["--profile", "*", "config", "--format", "json"]);
        serde_json::from_slice(&capture(&mut cmd)?).map_err(|e| invalid_json(e).into())
    }
}

fn identity(root: &Path, base: &str) -> String {
    let mut bytes = root.as_os_str().as_encoded_bytes().to_vec();
    bytes.push(0);
    bytes.extend(base.as_bytes());
    if let Ok(Some(worktree)) =
        crate::worktree::resolve(Some(&crate::worktree::Setting::Enabled(true)), root)
    {
        bytes.push(0);
        bytes.extend(worktree.key.as_bytes());
    }
    hash(&bytes)
}
fn project_lock(store: &Store, key: &str) -> crate::Result<File> {
    let parent = store
        .path()
        .parent()
        .ok_or_else(|| invalid("state directory missing"))?;
    fs::create_dir_all(parent)?;
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(parent.join(format!("compose-{key}.lock")))?;
    file.try_lock().map_err(|_| {
        invalid("another Nook Compose operation owns this project; retry after it finishes")
    })?;
    Ok(file)
}

pub(crate) fn claims(registry: &Registry, hostname: &str) -> bool {
    registry
        .compose_projects
        .values()
        .any(|p| p.routes.values().any(|r| r.alias.hostname == hostname))
}

pub(crate) fn write_status(registry: &Registry, output: &mut impl Write) -> io::Result<()> {
    for project in registry.compose_projects.values() {
        for endpoint in project.routes.values() {
            writeln!(
                output,
                "compose\t{}\t{}\t{}\t{}\t{}",
                endpoint.status,
                endpoint.alias.hostname,
                endpoint.alias.target,
                project.name,
                endpoint.service
            )?;
        }
    }
    Ok(())
}

pub(crate) fn execute(
    args: Args,
    global: &crate::config::GlobalConfig,
    output: &mut impl Write,
    errors: &mut impl Write,
) -> crate::Result<i32> {
    let store = Store::new(crate::state::state_path()?);
    let docker = Docker::current()?;
    let engine = docker.engine()?;
    // Down can recover a registered project without its original files.
    if let Action::Down(down) = &args.command {
        let root = fs::canonicalize(
            args.project_directory
                .as_ref()
                .cloned()
                .unwrap_or(std::env::current_dir()?),
        )?;
        let registry = store.load()?;
        let candidates: Vec<_> = registry
            .compose_projects
            .values()
            .filter(|p| {
                p.host == docker.host
                    && root == p.root
                    && args.project.as_ref().is_none_or(|name| name == &p.name)
            })
            .cloned()
            .collect();
        if candidates.len() == 1 {
            return down_project(
                &store,
                &docker,
                &engine,
                candidates[0].clone(),
                down,
                global,
                errors,
            );
        }
        if candidates.len() > 1 {
            return Err(invalid(
                "multiple registered projects; specify -p with the effective project name",
            )
            .into());
        }
    }
    let invocation = Invocation::resolve(&args)?;
    let initial = invocation.model(&docker, args.project.as_deref(), None)?;
    let base = initial["name"]
        .as_str()
        .ok_or_else(|| invalid("Compose did not return a project name"))?;
    let key = identity(&invocation.root, base);
    let name = args
        .project
        .clone()
        .unwrap_or_else(|| format!("{base}-nook-{}", &key[..12]));
    let _lock = project_lock(&store, &key)?;
    let model = invocation.model(&docker, Some(&name), None)?;
    if matches!(
        args.command,
        Action::Config | Action::Ps { .. } | Action::Logs { .. }
    ) {
        let mut cmd = invocation.command(&docker, Some(&name));
        match &args.command {
            Action::Config => {
                cmd.arg("config");
            }
            Action::Ps { all } => {
                cmd.arg("ps");
                if *all {
                    cmd.arg("--all");
                }
            }
            Action::Logs {
                follow,
                tail,
                services,
            } => {
                cmd.arg("logs");
                if *follow {
                    cmd.arg("--follow");
                }
                if let Some(tail) = tail {
                    cmd.args(["--tail", &tail.to_string()]);
                }
                cmd.args(services);
            }
            _ => unreachable!(),
        }
        return Ok(cmd.status()?.code().unwrap_or(1));
    }
    let registry = store.load()?;
    if registry
        .compose_projects
        .values()
        .any(|p| p.name == name && p.host == docker.host && p.identity != key)
    {
        return Err(invalid("Compose project belongs to another directory/worktree").into());
    }
    let namespace = crate::config::normalize_hostname(base.replace('_', "-").as_str())?
        .trim_end_matches(".localhost")
        .to_owned();
    let mut project = registry
        .compose_projects
        .get(&key)
        .cloned()
        .unwrap_or(Project {
            id: Uuid::new_v4(),
            identity: key.clone(),
            root: invocation.root.clone(),
            name: name.clone(),
            namespace: namespace.clone(),
            host: docker.host.clone(),
            engine: engine.clone(),
            routes: BTreeMap::new(),
            services: BTreeSet::new(),
        });
    if project.host != docker.host || project.engine != engine {
        return Err(
            invalid("Docker engine changed; refusing to reuse registered ownership").into(),
        );
    }
    if let Action::Down(down) = &args.command {
        drop(_lock);
        return down_project(&store, &docker, &engine, project, down, global, errors);
    }
    let Action::Up(up) = &args.command else {
        unreachable!()
    };
    let worktree_setting = if args.no_worktree {
        None
    } else {
        let in_git = Command::new("git")
            .arg("-C")
            .arg(&invocation.root)
            .args(["rev-parse", "--show-toplevel"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|s| s.success());
        if in_git || args.worktree.is_some() {
            Some(
                args.worktree
                    .as_ref()
                    .filter(|s| !s.is_empty())
                    .map_or(crate::worktree::Setting::Enabled(true), |s| {
                        crate::worktree::Setting::Named(s.clone())
                    }),
            )
        } else {
            None
        }
    };
    let worktree = crate::worktree::resolve(worktree_setting.as_ref(), &invocation.root)?;
    let selected_model: Value = serde_json::from_slice(&capture(
        invocation
            .command(&docker, Some(&name))
            .args(["config", "--format", "json"])
            .args(&up.services),
    )?)
    .map_err(invalid_json)?;
    let selected: BTreeSet<String> = selected_model["services"]
        .as_object()
        .ok_or_else(|| invalid("Compose selection has no services"))?
        .keys()
        .filter(|service| !up.no_deps || up.services.is_empty() || up.services.contains(service))
        .cloned()
        .collect();
    let endpoints = compose_config::endpoints(&model, &namespace, worktree.as_ref())?;
    if up.wait {
        let help = capture(
            invocation
                .command(&docker, Some(&name))
                .args(["up", "--help"]),
        )?;
        let help = String::from_utf8_lossy(&help);
        if !help.contains("--wait-timeout") {
            return Err(invalid(
                "Docker Compose does not support --wait-timeout; upgrade the Compose plugin",
            )
            .into());
        }
    }
    for service in &selected {
        if endpoints.values().any(|e| &e.service == service)
            && model["services"][service]["ports"]
                .as_array()
                .into_iter()
                .flatten()
                .any(|p| {
                    p["host_ip"].as_str().is_none_or(|ip| {
                        ip.parse::<std::net::IpAddr>()
                            .is_ok_and(|ip| ip.is_unspecified())
                    })
                })
        {
            writeln!(
                errors,
                "warning: {service} publishes on all host interfaces; Nook does not change firewall rules"
            )?;
        }
    }

    docker.containers(&project)?;
    validate_resources(&docker, &project, &model)?;
    let client = crate::caddy::Client::new(&global.caddy_admin)?;
    let caddy = client.fetch_config()?;
    let mut incoming = BTreeMap::new();
    for (id, mut endpoint) in endpoints
        .into_iter()
        .filter(|(_, e)| selected.contains(&e.service))
    {
        if let Some(old) = project.routes.get(&id) {
            if old.alias.hostname != endpoint.alias.hostname || old.alias.tls != endpoint.alias.tls
            {
                return Err(invalid("route name/TLS changed; run nook compose down for this service before changing it").into());
            }
            endpoint.alias.id = old.alias.id;
            endpoint.alias.target = old.alias.target.clone();
            endpoint.active = old.active;
        }
        if registry
            .aliases
            .values()
            .any(|a| a.hostname == endpoint.alias.hostname)
            || registry
                .leases
                .values()
                .any(|a| a.hostname == endpoint.alias.hostname)
            || registry
                .compose_projects
                .values()
                .filter(|p| p.identity != key)
                .any(|p| {
                    p.routes
                        .values()
                        .any(|e| e.alias.hostname == endpoint.alias.hostname)
                })
        {
            return Err(invalid(format!(
                "hostname {} is already owned",
                endpoint.alias.hostname
            ))
            .into());
        }
        if let Some(servers) = caddy
            .pointer("/apps/http/servers")
            .and_then(Value::as_object)
        {
            for server in servers.values() {
                if let Some(routes) = server["routes"].as_array() {
                    crate::caddy::reject_foreign_hostname_claims(
                        routes,
                        std::slice::from_ref(&endpoint.alias.hostname),
                    )?;
                }
            }
        }
        incoming.insert(id, endpoint);
    }
    if incoming.is_empty() && project.routes.is_empty() {
        return Err(invalid(
            "no HTTP endpoints selected; declare io.ntnook.route.<id>.port labels",
        )
        .into());
    }
    // Validate listeners before creating any Docker resources.
    crate::cli::with_caddy_routes(
        global,
        incoming.values().any(|e| e.alias.tls),
        incoming.values().any(|e| !e.alias.tls),
        |_| Ok(()),
    )?;
    let wanted: BTreeSet<_> = incoming.keys().cloned().collect();
    for (id, e) in &mut project.routes {
        if selected.contains(&e.service) && !wanted.contains(id) {
            e.desired = false;
        }
    }
    project.routes.extend(incoming);
    project.services.extend(selected.clone());
    {
        let _operations = store.lock_operations()?;
        // Recheck reservations under the global operation lock.
        let latest = store.load()?;
        if latest
            .compose_projects
            .values()
            .any(|p| p.host == project.host && p.name == project.name && p.identity != key)
        {
            return Err(invalid("Compose project was claimed by another operation").into());
        }
        for endpoint in project.routes.values() {
            if latest.aliases.contains_key(&endpoint.alias.hostname)
                || latest
                    .leases
                    .values()
                    .any(|l| l.hostname == endpoint.alias.hostname)
                || latest
                    .compose_projects
                    .values()
                    .filter(|p| p.identity != key)
                    .any(|p| {
                        p.routes
                            .values()
                            .any(|e| e.alias.hostname == endpoint.alias.hostname)
                    })
            {
                return Err(invalid("hostname claimed by another operation").into());
            }
        }
        save(&store, &project)?;
    }
    let override_path = store
        .path()
        .with_extension(format!("compose-{}.json", project.id));
    let mut override_value = json!({"services":{},"networks":{},"volumes":{}});
    for service in model["services"].as_object().unwrap().keys() {
        override_value["services"][service] =
            json!({"labels":{OWNER:project.id.to_string(),IDENTITY:project.identity}});
    }
    for kind in ["networks", "volumes"] {
        if let Some(resources) = model[kind].as_object() {
            for (name, resource) in resources {
                if resource["external"] != true {
                    override_value[kind][name] =
                        json!({"labels":{OWNER:project.id.to_string(),IDENTITY:project.identity}});
                }
            }
        }
    }
    fs::write(
        &override_path,
        serde_json::to_vec(&override_value).map_err(invalid_json)?,
    )?;
    let _override = Temporary(override_path.clone());
    // Validate Compose's merge and ensure original inputs did not change during preflight.
    invocation.model(&docker, Some(&name), Some(&override_path))?;
    if invocation.model(&docker, Some(&name), None)? != model {
        return Err(invalid("Compose inputs changed during validation; retry").into());
    }
    let mut command = invocation.command(&docker, Some(&name));
    command.arg("-f").arg(&override_path).arg("up");
    let detached = up.detach || up.wait;
    if detached {
        command.arg("--detach");
    }
    for (enabled, flag) in [
        (up.build, "--build"),
        (up.no_build, "--no-build"),
        (up.force_recreate, "--force-recreate"),
        (up.no_recreate, "--no-recreate"),
        (up.no_deps, "--no-deps"),
        (up.wait, "--wait"),
    ] {
        if enabled {
            command.arg(flag);
        }
    }
    if let Some(pull) = &up.pull {
        command.args(["--pull", pull]);
    }
    if up.wait {
        command.args(["--wait-timeout", &up.wait_timeout.to_string()]);
    }
    command.args(&up.services);
    writeln!(errors, "nook: compose_project={name} owner={}", project.id)?;
    let mut signals = crate::process::ForwardedSignals::new()?;
    let argv: Vec<OsString> = std::iter::once(command.get_program().to_owned())
        .chain(command.get_args().map(|v| v.to_owned()))
        .collect();
    let environment: Vec<_> = command
        .get_envs()
        .map(|(key, value)| (key.to_owned(), value.unwrap_or_default().to_owned()))
        .collect();
    let mut child = crate::process::spawn_managed_child_in(
        &argv,
        &environment,
        Uuid::new_v4(),
        Some(&invocation.cwd),
        None,
    )
    .map_err(|e| invalid(e.to_string()))?;
    let mut interrupted = false;
    let mut interruption = None;
    let started = Instant::now();
    let mut warned = false;
    let mut last_tick = Instant::now() - Duration::from_secs(2);
    let code = loop {
        for signal in signals.pending() {
            interrupted = true;
            interruption.get_or_insert_with(Instant::now);
            let _ = child.signal_foreground(signal);
        }
        if interruption.is_some_and(|when: Instant| when.elapsed() >= Duration::from_secs(2)) {
            // Container processes live in Docker, not in the CLI process tree.
            stop_selected(&docker, &project, &selected, 10)?;
            let _ = child.signal(crate::process::ProcessSignal::Kill);
            interruption = None;
        }
        if !detached && !warned && started.elapsed() >= Duration::from_secs(30) {
            let current = store.load()?;
            if current.compose_projects[&key]
                .routes
                .values()
                .any(|e| e.desired && !e.active)
            {
                writeln!(
                    errors,
                    "warning: HTTP endpoints are still starting; inspect application logs"
                )?;
            }
            warned = true;
        }
        if !detached && last_tick.elapsed() >= Duration::from_secs(1) {
            if let Err(e) = sync_project(&store, &key, global) {
                writeln!(errors, "warning: Compose reconciliation pending: {e}")?;
            }
            last_tick = Instant::now();
        }
        if let Some(code) = child.try_wait().map_err(|e| invalid(e.to_string()))? {
            break if interrupted { 130 } else { code };
        }
        thread::sleep(Duration::from_millis(100));
    };
    child.join_output();
    if interrupted {
        stop_selected(&docker, &project, &selected, 10)?;
    }
    if code != 0 || !detached {
        if let Err(error) = sync_project(&store, &key, global) {
            writeln!(
                errors,
                "warning: Compose cleanup/reconciliation pending: {error}"
            )?;
            return Ok(if code == 0 { 1 } else { code });
        }
        return Ok(code);
    }
    let deadline = Instant::now() + Duration::from_secs(if up.wait { up.wait_timeout } else { 30 });
    loop {
        sync_project(&store, &key, global)?;
        let current = store.load()?;
        let project = &current.compose_projects[&key];
        if project
            .routes
            .iter()
            .filter(|(id, _)| wanted.contains(*id))
            .all(|(_, e)| e.active && e.status == "ready")
        {
            write_status(&current, output)?;
            return Ok(0);
        }
        if Instant::now() >= deadline {
            write_status(&current, output)?;
            return Err(invalid("HTTP readiness timed out; containers retained, inspect docker compose logs and retry nook status/up").into());
        }
        thread::sleep(Duration::from_millis(250));
    }
}

struct Temporary(PathBuf);
impl Drop for Temporary {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}
fn save(store: &Store, project: &Project) -> crate::Result<()> {
    store.mutate(|r| {
        r.compose_projects
            .insert(project.identity.clone(), project.clone());
        Ok(())
    })?;
    Ok(())
}

fn validate_resources(docker: &Docker, project: &Project, model: &Value) -> crate::Result<()> {
    for (kind, command) in [("networks", "network"), ("volumes", "volume")] {
        if let Some(resources) = model[kind].as_object() {
            for resource in resources.values().filter(|n| n["external"] != true) {
                if let Some(name) = resource["name"].as_str() {
                    let ids =
                        capture(
                            docker
                                .command()
                                .args([command, "ls", "--format", "{{.Name}}"]),
                        )?;
                    if String::from_utf8_lossy(&ids).lines().any(|n| n == name) {
                        let value = docker.json(&[command, "inspect", name])?;
                        check_network(&value[0], project)?;
                    }
                }
            }
        }
    }
    Ok(())
}
fn check_network(network: &Value, project: &Project) -> crate::Result<()> {
    if network["Labels"][OWNER].as_str() != Some(project.id.to_string().as_str())
        || network["Labels"][IDENTITY].as_str() != Some(&project.identity)
    {
        return Err(invalid("refusing foreign Compose network").into());
    }
    Ok(())
}

fn stop_selected(
    docker: &Docker,
    project: &Project,
    selected: &BTreeSet<String>,
    timeout: u32,
) -> crate::Result<()> {
    for container in docker.containers(project)? {
        if selected.contains(
            container["Config"]["Labels"]["com.docker.compose.service"]
                .as_str()
                .unwrap_or_default(),
        ) {
            let id = container["Id"]
                .as_str()
                .ok_or_else(|| invalid("container ID missing"))?;
            let status = docker
                .command()
                .args(["stop", "--time", &timeout.to_string(), id])
                .status()?;
            if !status.success() {
                return Err(invalid("could not stop owned container").into());
            }
        }
    }
    Ok(())
}

fn down_project(
    store: &Store,
    docker: &Docker,
    engine: &str,
    mut project: Project,
    down: &Down,
    global: &crate::config::GlobalConfig,
    errors: &mut impl Write,
) -> crate::Result<i32> {
    let _lock = project_lock(store, &project.identity)?;
    // The caller resolved an identity before locking. An up may have completed
    // since then; never overwrite its newer services/routes with that snapshot.
    project = store
        .load()?
        .compose_projects
        .get(&project.identity)
        .cloned()
        .ok_or_else(|| invalid("Compose project is no longer registered"))?;
    if project.engine != engine {
        return Err(invalid("Docker engine identity changed; refusing teardown").into());
    }
    let containers = docker.containers(&project)?;
    let selected = if down.services.is_empty() {
        project.services.clone()
    } else {
        down.services.iter().cloned().collect()
    };
    if !selected.is_subset(&project.services) {
        return Err(invalid("down names a service not owned by this project").into());
    }
    for endpoint in project
        .routes
        .values_mut()
        .filter(|e| selected.contains(&e.service))
    {
        endpoint.desired = false;
    }
    {
        let _operations = store.lock_operations()?;
        save(store, &project)?;
    }
    let mut incomplete = false;
    if let Err(e) = sync_project(store, &project.identity, global) {
        incomplete = true;
        writeln!(errors, "warning: route removal pending: {e}")?;
    }
    for container in containers {
        if selected.contains(
            container["Config"]["Labels"]["com.docker.compose.service"]
                .as_str()
                .unwrap_or_default(),
        ) {
            let id = container["Id"]
                .as_str()
                .ok_or_else(|| invalid("container ID missing"))?;
            // Revalidate the immutable ID immediately before each destructive operation.
            let current = docker.json(&["inspect", id])?;
            check_container(&current[0], &project)?;
            if !docker
                .command()
                .args(["stop", "--time", &down.timeout.to_string(), id])
                .status()?
                .success()
            {
                return Err(invalid("owned container stop failed; retry down").into());
            }
            if !docker.command().args(["rm", id]).status()?.success() {
                return Err(invalid("owned container removal failed; retry down").into());
            }
        }
    }
    if down.services.is_empty() {
        let filter = format!("label=com.docker.compose.project={}", project.name);
        let ids = capture(
            docker
                .command()
                .args(["network", "ls", "-q", "--filter", &filter]),
        )?;
        for id in String::from_utf8_lossy(&ids).split_whitespace() {
            let value = docker.json(&["network", "inspect", id])?;
            check_network(&value[0], &project)?;
            if value[0]["Containers"]
                .as_object()
                .is_some_and(|c| !c.is_empty())
            {
                writeln!(errors, "warning: retaining network in use {id}")?;
                continue;
            }
            if !docker
                .command()
                .args(["network", "rm", id])
                .status()?
                .success()
            {
                incomplete = true;
            }
        }
    }
    if let Err(e) = sync_project(store, &project.identity, global) {
        incomplete = true;
        writeln!(errors, "warning: route removal pending: {e}")?;
    }
    Ok(if incomplete { 1 } else { 0 })
}

fn sync_project(
    store: &Store,
    key: &str,
    global: &crate::config::GlobalConfig,
) -> crate::Result<()> {
    let _operations = store.lock_operations()?;
    let registry = store.load()?;
    let project = &registry.compose_projects[key];
    if project.routes.is_empty() {
        return Ok(());
    }
    crate::cli::with_caddy_routes(
        global,
        project.routes.values().any(|e| e.alias.tls),
        project.routes.values().any(|e| !e.alias.tls),
        |routes| {
            let warnings = store.mutate(|r| {
                Ok(reconcile_one(
                    r.compose_projects.get_mut(key).unwrap(),
                    routes,
                ))
            })?;
            if !warnings.is_empty() {
                return Err(invalid(warnings.join("; ")).into());
            }
            Ok(())
        },
    )
}

pub(crate) fn reconcile(registry: &mut Registry, routes: &mut impl RouteBackend) -> Vec<String> {
    registry
        .compose_projects
        .values_mut()
        .flat_map(|project| reconcile_one(project, routes))
        .collect()
}

fn reconcile_one(project: &mut Project, routes: &mut impl RouteBackend) -> Vec<String> {
    if project.routes.is_empty() {
        return Vec::new();
    }
    let docker = Docker {
        host: project.host.clone(),
    };
    let observation = (|| -> crate::Result<Vec<Value>> {
        if docker.engine()? != project.engine {
            return Err(invalid("Docker engine identity changed").into());
        }
        docker.observe_containers(project)
    })();
    reconcile_observed(project, routes, observation)
}

fn reconcile_observed(
    project: &mut Project,
    routes: &mut impl RouteBackend,
    observation: crate::Result<Vec<Value>>,
) -> Vec<String> {
    let containers = match observation {
        Ok(c) => c,
        Err(e) => {
            for endpoint in project.routes.values_mut() {
                endpoint.status = "unknown".into();
            }
            return vec![format!("Compose {} observation pending: {e}", project.name)];
        }
    };
    let mut warnings = Vec::new();
    let ownership = project.clone();
    for endpoint in project.routes.values_mut() {
        let matches: Vec<_> = containers
            .iter()
            .filter(|c| {
                c["Config"]["Labels"]["com.docker.compose.service"].as_str()
                    == Some(&endpoint.service)
            })
            .collect();
        let observed = if matches
            .iter()
            .any(|container| check_container(container, &ownership).is_err())
        {
            Err(invalid(
                "foreign container replaced an owned service; withdrawing only the Nook route",
            )
            .into())
        } else if matches.len() > 1 {
            Err(invalid("multiple replicas are not supported").into())
        } else if let Some(container) = matches.first().filter(|c| c["State"]["Running"] == true) {
            compose_config::target(container, endpoint.port)
        } else {
            Ok(None)
        };
        let target = match observed {
            Ok(t) => t,
            Err(e) => {
                warnings.push(e.to_string());
                None
            }
        };
        if !endpoint.desired || target.is_none() {
            match routes.remove_if_owned(
                &endpoint.alias.hostname,
                endpoint.alias.id,
                endpoint.alias.tls,
            ) {
                Ok(()) => {
                    endpoint.active = false;
                    endpoint.status = "stopped".into();
                }
                Err(e) => {
                    endpoint.status = "removal-pending".into();
                    warnings.push(e.to_string());
                }
            }
            continue;
        }
        let target = target.unwrap();
        let changed = endpoint.alias.target != target;
        if changed && endpoint.active {
            if let Err(e) = routes.remove_if_owned(
                &endpoint.alias.hostname,
                endpoint.alias.id,
                endpoint.alias.tls,
            ) {
                warnings.push(e.to_string());
                continue;
            }
            endpoint.active = false;
        }
        endpoint.alias.target = target;
        let health = matches[0]["State"]["Health"]["Status"].as_str();
        let healthy = health.is_none_or(|h| h == "healthy");
        let ready = healthy
            && http_ready(
                &endpoint.alias.target,
                &endpoint.alias.hostname,
                endpoint.alias.preserve_host,
            );
        endpoint.status = if ready {
            "ready"
        } else if endpoint.active {
            "degraded"
        } else {
            "starting"
        }
        .into();
        if ready || endpoint.active {
            match routes.ensure(&crate::reconcile::route_for_alias(&endpoint.alias)) {
                Ok(()) => endpoint.active = true,
                Err(e) => {
                    endpoint.status = "route-pending".into();
                    warnings.push(e.to_string());
                }
            }
        }
    }
    project
        .routes
        .retain(|_, e| e.desired || e.active || e.status == "removal-pending");
    warnings
}

fn http_ready(target: &str, hostname: &str, preserve_host: bool) -> bool {
    let agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_millis(500)))
        .max_redirects(0)
        .http_status_as_error(false)
        .build()
        .new_agent();
    let mut request = agent.get(target);
    if preserve_host {
        request = request.header("Host", hostname);
    }
    request
        .call()
        .is_ok_and(|r| (200..500).contains(&r.status().as_u16()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::reconcile::{RouteError, RouteSpec};
    use clap::Parser;

    fn project() -> Project {
        let model = json!({"name":"app","services":{"web":{"ports":[{"target":8080}],"labels":{"io.ntnook.route.http.port":"8080","io.ntnook.route.http.tls":"false"}}}});
        Project {
            id: Uuid::new_v4(),
            identity: "identity".into(),
            root: "/project".into(),
            name: "app".into(),
            namespace: "app".into(),
            host: "unix:///docker.sock".into(),
            engine: "engine".into(),
            routes: compose_config::endpoints(&model, "app", None).unwrap(),
            services: BTreeSet::from(["web".into()]),
        }
    }
    fn container(project: &Project, port: u16) -> Value {
        json!({"Id":"owned-id", "Config":{"Labels":{OWNER:project.id.to_string(),IDENTITY:project.identity,"com.docker.compose.project":project.name,"com.docker.compose.service":"web"}},"State":{"Running":true},"NetworkSettings":{"Ports":{"8080/tcp":[{"HostIp":"127.0.0.1","HostPort":port.to_string()}]}}})
    }
    #[derive(Default)]
    struct Routes {
        ensured: Vec<RouteSpec>,
        removed: Vec<Uuid>,
        fail_remove: bool,
    }
    impl RouteBackend for Routes {
        fn ensure(&mut self, route: &RouteSpec) -> Result<(), RouteError> {
            self.ensured.push(route.clone());
            Ok(())
        }
        fn remove_if_owned(&mut self, _: &str, id: Uuid, _: bool) -> Result<(), RouteError> {
            if self.fail_remove {
                Err(RouteError("offline".into()))
            } else {
                self.removed.push(id);
                Ok(())
            }
        }
    }
    #[test]
    fn compose_is_reserved_and_options_are_bounded() {
        for argv in [
            vec!["nook", "compose", "up", "-d"],
            vec![
                "nook",
                "compose",
                "-f",
                "two words.yaml",
                "--profile",
                "debug",
                "--env-file",
                "local env",
                "up",
                "--wait",
                "web",
            ],
            vec!["nook", "compose", "down", "web"],
            vec!["nook", "compose", "logs", "--follow", "web"],
        ] {
            assert!(crate::cli::Cli::try_parse_from(argv).is_ok());
        }
        for argv in [
            vec!["nook", "compose", "up", "--scale", "web=2"],
            vec!["nook", "compose", "down", "-v"],
            vec!["nook", "compose", "restart"],
            vec!["nook", "compose", "up", "--remove-orphans"],
        ] {
            assert!(crate::cli::Cli::try_parse_from(argv).is_err());
        }
    }
    #[test]
    fn ownership_requires_both_registry_identity_and_owner() {
        let p = project();
        let mut c = container(&p, 12345);
        assert!(check_container(&c, &p).is_ok());
        c["Config"]["Labels"][OWNER] = json!(Uuid::new_v4().to_string());
        assert!(check_container(&c, &p).is_err());
    }
    #[test]
    fn uncertain_engine_never_deletes_or_restores_an_alias() {
        let mut p = project();
        p.routes.get_mut("web/http").unwrap().active = true;
        let mut routes = Routes::default();
        assert!(
            !reconcile_observed(&mut p, &mut routes, Err(invalid("offline").into())).is_empty()
        );
        assert!(routes.ensured.is_empty() && routes.removed.is_empty());
        assert!(p.routes["web/http"].active);
        assert_eq!(p.routes["web/http"].status, "unknown");
    }
    #[test]
    fn failed_cleanup_retains_durable_intent_even_before_active_was_saved() {
        let mut p = project();
        p.routes.get_mut("web/http").unwrap().desired = false;
        let mut routes = Routes {
            fail_remove: true,
            ..Routes::default()
        };
        assert!(!reconcile_observed(&mut p, &mut routes, Ok(vec![])).is_empty());
        assert_eq!(p.routes.len(), 1);
        routes.fail_remove = false;
        assert!(reconcile_observed(&mut p, &mut routes, Ok(vec![])).is_empty());
        assert!(p.routes.is_empty());
    }
    #[test]
    fn foreign_recreation_withdraws_only_the_owned_route() {
        let mut p = project();
        p.routes.get_mut("web/http").unwrap().active = true;
        let id = p.routes["web/http"].alias.id;
        let mut c = container(&p, 12345);
        c["Config"]["Labels"][OWNER] = Value::Null;
        let mut routes = Routes::default();
        assert!(!reconcile_observed(&mut p, &mut routes, Ok(vec![c])).is_empty());
        assert_eq!(routes.removed, vec![id]);
        assert!(routes.ensured.is_empty());
        assert!(!p.routes["web/http"].active);
    }
    #[test]
    fn recreate_discovers_port_and_keeps_route_uuid() {
        use std::net::TcpListener;
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0; 2048];
            assert!(stream.read(&mut request).unwrap() > 0);
            stream
                .write_all(b"HTTP/1.1 204 No Content\r\nConnection: close\r\n\r\n")
                .unwrap();
        });
        let mut p = project();
        let endpoint = p.routes.get_mut("web/http").unwrap();
        endpoint.active = true;
        endpoint.alias.target = "http://127.0.0.1:1/".into();
        let id = endpoint.alias.id;
        let c = container(&p, port);
        let mut routes = Routes::default();
        assert!(reconcile_observed(&mut p, &mut routes, Ok(vec![c])).is_empty());
        server.join().unwrap();
        assert_eq!(routes.removed, vec![id]);
        assert_eq!(routes.ensured[0].owner_id, id);
        assert_eq!(
            routes.ensured[0].target,
            format!("http://127.0.0.1:{port}/")
        );
        assert_eq!(p.routes["web/http"].status, "ready");
    }
    #[test]
    fn down_revalidates_the_registered_snapshot_after_locking() {
        let root = std::env::temp_dir().join(format!("nook-compose-lock-{}", Uuid::new_v4()));
        let store = Store::new(root.join("state.json"));
        let stale = project();
        let mut latest = stale.clone();
        latest.engine = "changed-after-resolution".into();
        latest.services.insert("new-service".into());
        save(&store, &latest).unwrap();
        let result = down_project(
            &store,
            &Docker {
                host: "must-not-be-contacted".into(),
            },
            &stale.engine.clone(),
            stale,
            &Down {
                timeout: 10,
                services: Vec::new(),
            },
            &crate::config::GlobalConfig::default(),
            &mut Vec::new(),
        );
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("engine identity changed")
        );
        assert_eq!(
            store.load().unwrap().compose_projects[&latest.identity],
            latest
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn manual_alias_force_cannot_take_a_compose_reservation() {
        let root = std::env::temp_dir().join(format!("nook-compose-unit-{}", Uuid::new_v4()));
        let store = Store::new(root.join("state.json"));
        let p = project();
        let hostname = p.routes["web/http"].alias.hostname.clone();
        save(&store, &p).unwrap();
        let mut routes = Routes::default();
        assert!(
            crate::reconcile::set_alias(
                &store,
                &mut routes,
                crate::reconcile::AliasRequest {
                    hostname: hostname.clone(),
                    target: "http://127.0.0.1:1/".into(),
                    scheme: crate::state::Scheme::Http,
                    tls: false,
                    preserve_host: false,
                    force: true
                }
            )
            .is_err()
        );
        assert!(crate::reconcile::remove_alias(&store, &mut routes, &hostname).is_err());
        assert!(routes.ensured.is_empty() && routes.removed.is_empty());
        fs::remove_dir_all(root).unwrap();
    }
}
