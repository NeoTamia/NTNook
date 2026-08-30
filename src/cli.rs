//! Command-line parsing, terminal output, and exit-code policy.

use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

use clap::{Args, CommandFactory, Parser, Subcommand, ValueEnum};
use clap_complete::{Generator, Shell};
use sha2::{Digest, Sha256};

use crate::reconcile::RouteBackend;

#[derive(Debug, Parser)]
#[command(
    name = "nook",
    version,
    about = "Expose local services through stable *.localhost domains",
    arg_required_else_help = true
)]
pub(crate) struct Cli {
    /// Use a Caddy Admin API Unix socket instead of the configured address.
    #[arg(long, value_name = "PATH")]
    pub(crate) caddy_socket: Option<String>,
    #[command(subcommand)]
    pub(crate) command: Command,
}

#[derive(Debug, Subcommand)]
pub(crate) enum Command {
    /// Create a project configuration in the current directory.
    Init(InitArgs),
    /// Run a command behind a temporary Nook route.
    Run(RunArgs),
    /// Manage persistent aliases.
    Alias(AliasArgs),
    /// List managed runs and aliases.
    List,
    /// Diagnose Nook and Caddy state.
    Status,
    /// Stop a managed run.
    Stop(StopArgs),
    /// Remove stale leases and reconcile routes.
    Prune,
    /// Manage Caddy's local certificate authority.
    Ca(CaArgs),
    /// Create and inspect Nook's global configuration.
    Config(ConfigArgs),
    /// Generate shell completion scripts.
    Completions(CompletionsArgs),
    /// Update the nook binary from GitHub Releases.
    Update(UpdateArgs),
}

#[derive(Debug, Args)]
pub(crate) struct InitArgs {
    /// Create nook.local.toml instead of nook.toml.
    #[arg(long)]
    pub(crate) local: bool,
    /// Print the configuration without creating a file.
    #[arg(long, conflicts_with = "force")]
    pub(crate) print: bool,
    /// Replace an existing regular configuration file.
    #[arg(long)]
    pub(crate) force: bool,
    /// Route name; .localhost is appended automatically.
    #[arg(long)]
    pub(crate) name: Option<String>,
    /// Expose the route over HTTP instead of HTTPS.
    #[arg(long)]
    pub(crate) no_tls: bool,
    /// Preferred application port.
    #[arg(long, value_name = "PORT")]
    pub(crate) app_port: Option<u16>,
    /// Fail rather than falling back when the preferred port is occupied.
    #[arg(long, requires = "app_port")]
    pub(crate) strict_port: bool,
    /// Seconds before warning that the application is not ready.
    #[arg(long, value_name = "SECONDS")]
    pub(crate) readiness_warn_after: Option<u64>,
    /// Child argv to store in the project configuration.
    #[arg(last = true, value_name = "COMMAND")]
    pub(crate) command: Vec<OsString>,
}

#[derive(Debug, Args)]
pub(crate) struct UpdateArgs {
    /// Report whether an update is available without installing it.
    #[arg(long)]
    pub(crate) check: bool,
    /// Reinstall the latest release even if this version is already current.
    #[arg(long)]
    pub(crate) force: bool,
}

#[derive(Debug, Args)]
pub(crate) struct CompletionsArgs {
    /// Shell whose completion script should be generated.
    pub(crate) shell: CompletionShell,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub(crate) enum CompletionShell {
    Bash,
    Zsh,
}

#[derive(Debug, Args)]
pub(crate) struct ConfigArgs {
    #[command(subcommand)]
    pub(crate) command: ConfigCommand,
}

#[derive(Debug, Subcommand)]
pub(crate) enum ConfigCommand {
    /// Create the global configuration with safe defaults.
    Init(ConfigInitArgs),
    /// Print the effective global configuration with defaults applied.
    Show,
    /// Print the path to the global configuration file.
    Path,
    /// Set one global configuration value.
    Set(ConfigSetArgs),
}

#[derive(Debug, Args)]
pub(crate) struct ConfigInitArgs {
    /// Configure Caddy's Admin API Unix socket.
    #[arg(long, value_name = "PATH")]
    pub(crate) caddy_socket: Option<String>,
    /// Replace an existing regular configuration file.
    #[arg(long)]
    pub(crate) force: bool,
}

#[derive(Debug, Args)]
pub(crate) struct ConfigSetArgs {
    pub(crate) key: ConfigKey,
    /// New value. Use `auto` to clear a server override; separate IP ranges with commas.
    pub(crate) value: String,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub(crate) enum ConfigKey {
    CaddyAdmin,
    HttpsServer,
    HttpServer,
    RunBindAddress,
    CaddyLoopbackHost,
    CaddyClientIpRanges,
}

#[derive(Debug, Args)]
pub(crate) struct CaArgs {
    #[command(subcommand)]
    pub(crate) command: CaCommand,
}

#[derive(Debug, Subcommand)]
pub(crate) enum CaCommand {
    /// Export Caddy's public local CA certificate without installing it.
    Export(CaExportArgs),
}

#[derive(Debug, Args)]
pub(crate) struct CaExportArgs {
    /// Destination PEM file; its parent directory must already exist.
    pub(crate) path: PathBuf,
    /// Replace an existing regular file.
    #[arg(long)]
    pub(crate) force: bool,
}

#[derive(Debug, Args)]
pub(crate) struct RunArgs {
    /// Route name when using `nook run`.
    #[arg(long)]
    pub(crate) name: Option<String>,
    /// Expose the route over HTTP instead of HTTPS.
    #[arg(long)]
    pub(crate) no_tls: bool,
    /// Preferred application port.
    #[arg(long, value_name = "PORT")]
    pub(crate) app_port: Option<u16>,
    /// Fail rather than falling back when the preferred port is occupied.
    #[arg(long, requires = "app_port")]
    pub(crate) strict_port: bool,
    /// Replace an existing Nook-owned route.
    #[arg(long)]
    pub(crate) force: bool,
    /// Explicit project configuration file.
    #[arg(long, value_name = "PATH")]
    pub(crate) config: Option<PathBuf>,
    /// Apply nook.local.toml next to the explicit project configuration.
    #[arg(long, requires = "config")]
    pub(crate) local: bool,
    /// Seconds before warning that the application is not ready.
    #[arg(long, value_name = "SECONDS")]
    pub(crate) readiness_warn_after: Option<u64>,
    /// Child argv, preserved exactly and never passed through a shell.
    #[arg(last = true, value_name = "COMMAND")]
    pub(crate) command: Vec<OsString>,
}

#[derive(Debug, Args)]
pub(crate) struct AliasArgs {
    #[command(subcommand)]
    pub(crate) command: AliasCommand,
}

#[derive(Debug, Subcommand)]
pub(crate) enum AliasCommand {
    /// Create or replace a persistent alias.
    Set(AliasSetArgs),
    /// Remove a persistent alias.
    Remove(AliasRemoveArgs),
    /// List persistent aliases.
    List,
}

#[derive(Debug, Args)]
pub(crate) struct AliasSetArgs {
    /// Stable route name; `.localhost` is appended automatically.
    pub(crate) name: String,
    /// Upstream port or absolute HTTP(S) URL.
    pub(crate) target: String,
    /// Expose the alias over HTTP instead of HTTPS.
    #[arg(long)]
    pub(crate) no_tls: bool,
    /// Pass the requested `.localhost` Host header to the upstream.
    #[arg(long)]
    pub(crate) preserve_host: bool,
    /// Replace an existing Nook-owned route.
    #[arg(long)]
    pub(crate) force: bool,
}

#[derive(Debug, Args)]
pub(crate) struct AliasRemoveArgs {
    /// Alias name to remove.
    pub(crate) name: String,
}

#[derive(Debug, Args)]
pub(crate) struct StopArgs {
    /// Managed run name to stop.
    pub(crate) name: String,
    /// Send SIGKILL if the same process remains alive after two seconds.
    #[arg(long)]
    pub(crate) force: bool,
}

pub(crate) fn run() -> crate::Result<i32> {
    let cli = parse_from(std::env::args_os())?;
    let stdout = io::stdout();
    let stderr = io::stderr();
    execute(cli, &mut stdout.lock(), &mut stderr.lock())
}

fn execute(cli: Cli, output: &mut impl Write, errors: &mut impl Write) -> crate::Result<i32> {
    let Cli {
        caddy_socket,
        command,
    } = cli;
    if !matches!(command, Command::Completions(_) | Command::Update(_)) {
        crate::update::warn_if_available(errors);
    }
    let command = match command {
        Command::Init(arguments) => {
            return init_command(arguments, output, errors).map(|()| 0);
        }
        Command::Update(arguments) => {
            return update_command(arguments, output, errors);
        }
        Command::Completions(arguments) => {
            completions_command(arguments, output)?;
            return Ok(0);
        }
        Command::Config(arguments) => {
            if caddy_socket.is_some() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "--caddy-socket must be placed after `config init` for configuration commands",
                )
                .into());
            }
            return config_command(arguments, output).map(|()| 0);
        }
        command => command,
    };
    let mut global = crate::config::load_global()?;
    if let Some(socket) = caddy_socket {
        global.caddy_admin = format!("unix/{socket}");
    }
    if !matches!(&command, Command::Prune | Command::Ca(_)) {
        reconcile_before_command(&global, errors)?;
    }
    match command {
        Command::Alias(AliasArgs {
            command: AliasCommand::Set(arguments),
        }) => set_alias_command(arguments, &global, output, errors).map(|()| 0),
        Command::Alias(AliasArgs {
            command: AliasCommand::Remove(arguments),
        }) => remove_alias_command(arguments, &global, output, errors).map(|()| 0),
        Command::Alias(AliasArgs {
            command: AliasCommand::List,
        }) => list_alias_command(output).map(|()| 0),
        Command::List => list_command(output).map(|()| 0),
        Command::Status => status_command(&global, output, errors).map(|()| 0),
        Command::Prune => prune_command(&global, output, errors).map(|()| 0),
        Command::Run(arguments) => run_command(arguments, &global, errors),
        Command::Stop(arguments) => stop_command(arguments, output).map(|()| 0),
        Command::Ca(CaArgs {
            command: CaCommand::Export(arguments),
        }) => ca_export_command(arguments, &global, output).map(|()| 0),
        Command::Config(_) => unreachable!("configuration commands return before loading Caddy"),
        Command::Init(_) | Command::Completions(_) | Command::Update(_) => {
            unreachable!("commands handled before global configuration return above")
        }
    }
}

fn init_command(
    arguments: InitArgs,
    output: &mut impl Write,
    errors: &mut impl Write,
) -> crate::Result<()> {
    let current_directory = std::env::current_dir()?;
    let contents = project_config_template(&arguments, &current_directory)?;
    if arguments.print {
        write!(output, "{contents}")?;
        return Ok(());
    }

    let filename = if arguments.local {
        "nook.local.toml"
    } else {
        "nook.toml"
    };
    let path = current_directory.join(filename);
    write_project_config(&path, contents.as_bytes(), arguments.force)?;
    writeln!(output, "created {}", path.display())?;
    if arguments.local && !local_config_is_ignored(&current_directory) {
        writeln!(
            errors,
            "warning: nook.local.toml may contain workstation-specific settings; add /nook.local.toml to .gitignore"
        )?;
    }
    Ok(())
}

fn project_config_template(arguments: &InitArgs, directory: &Path) -> crate::Result<String> {
    let mut contents = String::from("format_version = 1\n\n");
    let name = match arguments.name.as_deref() {
        Some(name) => crate::config::project_name(name)?,
        None => crate::config::infer_project_name(directory)?,
    };
    if arguments.local && arguments.name.is_none() {
        contents.push_str("# Override the shared route name on this workstation.\n");
        contents.push_str(&format!("# name = {}\n\n", toml_string(&name)));
    } else {
        contents.push_str("# Domain exposed as <name>.localhost.\n");
        contents.push_str(&format!("name = {}\n\n", toml_string(&name)));
    }

    if arguments.command.is_empty() {
        contents.push_str(
            "# Command started by `nook run`.\n# command = [\"pnpm\", \"run\", \"dev\"]\n\n",
        );
    } else {
        let command = arguments
            .command
            .iter()
            .map(|value| {
                value
                    .to_str()
                    .map(|value| toml::Value::String(value.to_owned()))
                    .ok_or_else(|| {
                        io::Error::new(
                            io::ErrorKind::InvalidInput,
                            "project commands must contain valid UTF-8",
                        )
                    })
            })
            .collect::<Result<Vec<_>, _>>()?;
        contents.push_str("# Command started by `nook run`.\n");
        contents.push_str(&format!("command = {}\n\n", toml::Value::Array(command)));
    }
    if arguments.no_tls {
        contents.push_str("# Expose the application over HTTPS.\ntls = false\n\n");
    } else {
        contents.push_str("# Expose the application over HTTPS.\n# tls = true\n\n");
    }
    match arguments.app_port {
        Some(port) => contents.push_str(&format!(
            "# Preferred application port.\napp_port = {port}\n\n"
        )),
        None => contents.push_str("# Preferred application port.\n# app_port = 5173\n\n"),
    }
    if arguments.strict_port {
        contents.push_str("# Fail when the preferred port is unavailable.\nstrict_port = true\n\n");
    } else {
        contents
            .push_str("# Fail when the preferred port is unavailable.\n# strict_port = false\n\n");
    }
    match arguments.readiness_warn_after {
        Some(seconds) => contents.push_str(&format!(
            "# Delay before the readiness warning.\nreadiness_warn_after_seconds = {seconds}\n"
        )),
        None => contents.push_str(
            "# Delay before the readiness warning.\n# readiness_warn_after_seconds = 30\n",
        ),
    }
    Ok(contents)
}

fn toml_string(value: &str) -> String {
    toml::Value::String(value.to_owned()).to_string()
}

fn write_project_config(path: &Path, contents: &[u8], force: bool) -> io::Result<()> {
    if let Ok(metadata) = fs::symlink_metadata(path) {
        if !force {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                format!(
                    "{} already exists; use --force to replace it",
                    path.display()
                ),
            ));
        }
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("{} is not a regular file", path.display()),
            ));
        }
    }
    let temporary = path.with_file_name(format!(
        ".{}.{}.tmp",
        path.file_name()
            .unwrap_or_else(|| OsStr::new("nook.toml"))
            .to_string_lossy(),
        uuid::Uuid::new_v4()
    ));
    let result = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o644)
            .open(&temporary)?;
        file.write_all(contents)?;
        file.sync_all()?;
        if force {
            fs::rename(&temporary, path)?;
        } else {
            fs::hard_link(&temporary, path)?;
            fs::remove_file(&temporary)?;
        }
        Ok(())
    })();
    if temporary.exists() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

fn local_config_is_ignored(directory: &Path) -> bool {
    std::process::Command::new("git")
        .arg("-C")
        .arg(directory)
        .args(["check-ignore", "--quiet", "nook.local.toml"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

fn update_command(
    arguments: UpdateArgs,
    output: &mut impl Write,
    errors: &mut impl Write,
) -> crate::Result<i32> {
    Ok(crate::update::perform(
        arguments.check,
        arguments.force,
        output,
        errors,
    )?)
}

fn completions_command(arguments: CompletionsArgs, output: &mut impl Write) -> io::Result<()> {
    let shell = match arguments.shell {
        CompletionShell::Bash => Shell::Bash,
        CompletionShell::Zsh => Shell::Zsh,
    };
    let mut command = Cli::command();
    command.set_bin_name("nook");
    command.build();
    shell.try_generate(&command, output)
}

fn config_command(arguments: ConfigArgs, output: &mut impl Write) -> crate::Result<()> {
    match arguments.command {
        ConfigCommand::Init(arguments) => {
            let mut config = crate::config::GlobalConfig::default();
            if let Some(socket) = arguments.caddy_socket {
                config.set_caddy_admin(format!("unix/{socket}"));
            }
            let path = crate::config::write_global(&config, arguments.force)?;
            writeln!(output, "created {}", path.display())?;
        }
        ConfigCommand::Show => {
            let config = crate::config::load_global()?;
            write!(output, "{}", crate::config::format_global(&config)?)?;
        }
        ConfigCommand::Path => {
            writeln!(output, "{}", crate::config::global_config_path()?.display())?;
        }
        ConfigCommand::Set(arguments) => {
            let mut config = crate::config::load_global()?;
            set_config_value(&mut config, arguments)?;
            let path = crate::config::write_global(&config, true)?;
            writeln!(output, "updated {}", path.display())?;
        }
    }
    Ok(())
}

fn set_config_value(
    config: &mut crate::config::GlobalConfig,
    arguments: ConfigSetArgs,
) -> crate::Result<()> {
    let ConfigSetArgs { key, value } = arguments;
    match key {
        ConfigKey::CaddyAdmin => config.set_caddy_admin(value),
        ConfigKey::HttpsServer => config.set_https_server(server_override(value)),
        ConfigKey::HttpServer => config.set_http_server(server_override(value)),
        ConfigKey::RunBindAddress => config.set_run_bind_address(value.parse().map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "run-bind-address must be an IP address",
            )
        })?),
        ConfigKey::CaddyLoopbackHost => config.set_caddy_loopback_host(value),
        ConfigKey::CaddyClientIpRanges => config.set_caddy_client_ip_ranges(
            value
                .split(',')
                .map(str::trim)
                .filter(|range| !range.is_empty())
                .map(str::to_owned)
                .collect(),
        ),
    }
    crate::config::format_global(config)?;
    Ok(())
}

fn server_override(value: String) -> Option<String> {
    (!matches!(value.as_str(), "auto" | "none")).then_some(value)
}

fn ca_export_command(
    arguments: CaExportArgs,
    global: &crate::config::GlobalConfig,
    output: &mut impl Write,
) -> crate::Result<()> {
    let client = crate::caddy::Client::new(&global.caddy_admin)?;
    let (pem, der) = crate::caddy::canonical_local_ca(&client.fetch_local_ca()?)?;
    write_certificate(&arguments.path, pem.as_bytes(), arguments.force)?;
    writeln!(
        output,
        "exported Caddy local CA to {}",
        arguments.path.display()
    )?;
    writeln!(output, "sha256={}", sha256_hex(&der))?;
    Ok(())
}

fn write_certificate(path: &Path, contents: &[u8], force: bool) -> io::Result<()> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    if !parent.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!(
                "certificate parent directory {} does not exist",
                parent.display()
            ),
        ));
    }
    if let Ok(metadata) = fs::symlink_metadata(path) {
        if !force {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                format!(
                    "{} already exists; use --force to replace it",
                    path.display()
                ),
            ));
        }
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("{} is not a regular file", path.display()),
            ));
        }
    }
    let name = path.file_name().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "certificate path has no filename",
        )
    })?;
    let temporary = parent.join(format!(
        ".{}.{}.tmp",
        name.to_string_lossy(),
        uuid::Uuid::new_v4()
    ));
    let result = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o644)
            .open(&temporary)?;
        file.write_all(contents)?;
        file.sync_all()?;
        if force {
            fs::rename(&temporary, path)?;
        } else {
            fs::hard_link(&temporary, path)?;
            fs::remove_file(&temporary)?;
        }
        Ok(())
    })();
    if temporary.exists() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

fn run_command(
    arguments: RunArgs,
    global: &crate::config::GlobalConfig,
    errors: &mut impl Write,
) -> crate::Result<i32> {
    let mut config = crate::config::resolve_run(&arguments, &std::env::current_dir()?)?;
    if let Some(path) = &config.ignored_local_config {
        writeln!(
            errors,
            "warning: ignoring local configuration {}; add --local to apply it",
            path.display()
        )?;
    }
    config.bind_address = global.run_bind_address;
    let store = state_store()?;
    with_caddy_routes(global, config.tls, !config.tls, |routes| {
        let mut running = crate::process::start_run(&config, &store, routes)?;
        let scheme = if config.tls { "https" } else { "http" };
        writeln!(
            errors,
            "nook: domain={} url={scheme}://{} port={}",
            running.hostname, running.hostname, running.port
        )?;
        if let Some(warning) = &running.warning {
            writeln!(errors, "warning: {warning}")?;
        }
        let _ready = running.wait_for_readiness(&store, |warning| {
            let _ = writeln!(errors, "warning: {warning}");
        })?;
        let outcome = running.finish(&store, routes)?;
        for warning in outcome.warnings {
            writeln!(errors, "warning: {warning}")?;
        }
        Ok(outcome.exit_code)
    })
}

fn stop_command(arguments: StopArgs, output: &mut impl Write) -> crate::Result<()> {
    let hostname = crate::config::normalize_hostname(&arguments.name)?;
    let store = state_store()?;
    let mut system = crate::process::LinuxStopSystem;
    crate::process::stop_managed(&store, &hostname, arguments.force, &mut system)?;
    writeln!(output, "sent SIGTERM to {hostname}")?;
    Ok(())
}

fn set_alias_command(
    arguments: AliasSetArgs,
    global: &crate::config::GlobalConfig,
    output: &mut impl Write,
    errors: &mut impl Write,
) -> crate::Result<()> {
    let hostname = crate::config::normalize_hostname(&arguments.name)?;
    let upstream = crate::caddy::normalize_upstream(&arguments.target)?;
    let scheme = match upstream.url.scheme() {
        "https" => crate::state::Scheme::Https,
        _ => crate::state::Scheme::Http,
    };
    let request = crate::reconcile::AliasRequest {
        hostname,
        target: upstream.url.to_string(),
        scheme,
        tls: !arguments.no_tls,
        preserve_host: arguments.preserve_host,
        force: arguments.force,
    };
    let store = crate::state::Store::new(crate::state::state_path()?);
    with_caddy_routes(global, request.tls, !request.tls, |routes| {
        let outcome = crate::reconcile::set_alias(&store, routes, request)?;
        for warning in outcome.warnings {
            writeln!(errors, "warning: {warning}")?;
        }
        writeln!(
            output,
            "{} -> {}",
            outcome.alias.hostname, outcome.alias.target
        )?;
        Ok(())
    })
}

fn remove_alias_command(
    arguments: AliasRemoveArgs,
    global: &crate::config::GlobalConfig,
    output: &mut impl Write,
    errors: &mut impl Write,
) -> crate::Result<()> {
    let hostname = crate::config::normalize_hostname(&arguments.name)?;
    let store = crate::state::Store::new(crate::state::state_path()?);
    let aliases = crate::reconcile::list_aliases(&store)?;
    let Some(alias) = aliases.iter().find(|alias| alias.hostname == hostname) else {
        writeln!(output, "alias {hostname} is not configured")?;
        return Ok(());
    };
    with_caddy_routes(global, alias.tls, !alias.tls, |routes| {
        for warning in crate::reconcile::remove_alias(&store, routes, &hostname)? {
            writeln!(errors, "warning: {warning}")?;
        }
        writeln!(output, "removed {hostname}")?;
        Ok(())
    })
}

fn list_alias_command(output: &mut impl Write) -> crate::Result<()> {
    let store = crate::state::Store::new(crate::state::state_path()?);
    for alias in crate::reconcile::list_aliases(&store)? {
        writeln!(output, "{} -> {}", alias.hostname, alias.target)?;
    }
    Ok(())
}

fn list_command(output: &mut impl Write) -> crate::Result<()> {
    let registry = state_store()?.load()?;
    write_registry_list(&registry, output)
}

fn write_registry_list(
    registry: &crate::state::Registry,
    output: &mut impl Write,
) -> crate::Result<()> {
    let mut leases: Vec<_> = registry.leases.values().collect();
    leases.sort_by(|left, right| left.hostname.cmp(&right.hostname));
    for lease in leases {
        let state = match lease.state {
            crate::state::LeaseState::Starting => "starting",
            crate::state::LeaseState::Ready => "ready",
        };
        writeln!(output, "run\t{state}\t{}\t{}", lease.hostname, lease.target)?;
    }
    for alias in registry.aliases.values() {
        writeln!(
            output,
            "alias\tpersistent\t{}\t{}",
            alias.hostname, alias.target
        )?;
    }
    Ok(())
}

fn status_command(
    global: &crate::config::GlobalConfig,
    output: &mut impl Write,
    errors: &mut impl Write,
) -> crate::Result<()> {
    let registry = state_store()?.load()?;
    let client = crate::caddy::Client::new(&global.caddy_admin)?;
    let config = client.fetch_config()?;
    let selection = available_servers(global, &config)?;
    let inspection = crate::caddy::inspect_managed(&config, &selection)?;
    writeln!(output, "caddy\tok")?;
    writeln!(output, "run_bind_address\t{}", global.run_bind_address)?;
    writeln!(
        output,
        "caddy_loopback_host\t{}",
        global.caddy_loopback_host
    )?;
    writeln!(
        output,
        "caddy_client_ip_ranges\t{}",
        global.caddy_client_ip_ranges.join(",")
    )?;
    writeln!(
        output,
        "https_server\t{}",
        selection.https.as_deref().unwrap_or("not required")
    )?;
    writeln!(
        output,
        "http_server\t{}",
        selection.http.as_deref().unwrap_or("not required")
    )?;
    writeln!(
        output,
        "https_container\t{}",
        if inspection.https_container {
            "present"
        } else {
            "absent"
        }
    )?;
    writeln!(
        output,
        "http_container\t{}",
        if inspection.http_container {
            "present"
        } else {
            "absent"
        }
    )?;
    let drift = drift_messages(&registry, &inspection);
    writeln!(
        output,
        "drift\t{}",
        if drift.is_empty() {
            "clean"
        } else {
            "detected"
        }
    )?;
    for warning in drift {
        writeln!(errors, "warning: {warning}")?;
    }
    let local_ca = client.fetch_local_ca()?;
    let (_, local_ca_der) = crate::caddy::canonical_local_ca(&local_ca)?;
    writeln!(output, "local_ca_sha256\t{}", sha256_hex(&local_ca_der))?;
    let trusted = crate::caddy::local_ca_is_trusted(&local_ca)?;
    writeln!(
        output,
        "local_ca\t{}",
        if trusted { "trusted" } else { "not trusted" }
    )?;
    if !trusted {
        writeln!(
            errors,
            "warning: Caddy's local CA is not trusted; export it with `nook ca export caddy-local-ca.pem` and install it explicitly (native Caddy users may run `{}`)",
            client.trust_command(),
        )?;
    }
    Ok(())
}

fn prune_command(
    global: &crate::config::GlobalConfig,
    output: &mut impl Write,
    errors: &mut impl Write,
) -> crate::Result<()> {
    let store = state_store()?;
    let _operations = store.lock_operations()?;
    let registry = store.load()?;
    let client = crate::caddy::Client::new(&global.caddy_admin)?;
    let config = client.fetch_config()?;
    let selection = available_servers(global, &config)?;
    let inspection = crate::caddy::inspect_managed(&config, &selection)?;
    let mut routes = crate::caddy::ManagedCaddyRoutes {
        client: &client,
        https_server: selection.https.as_deref(),
        http_server: selection.http.as_deref(),
        loopback_host: &global.caddy_loopback_host,
        client_ip_ranges: &global.caddy_client_ip_ranges,
    };
    let expected = expected_routes(&registry);
    let mut removed_orphans = 0;
    for observed in inspection.routes {
        if !expected.contains_key(&observed.owner_id) {
            match routes.remove_if_owned(&observed.hostname, observed.owner_id, observed.tls) {
                Ok(()) => removed_orphans += 1,
                Err(error) => writeln!(
                    errors,
                    "warning: cleanup of {} is pending: {error}",
                    observed.hostname
                )?,
            }
        }
    }
    let report = reconcile_and_record(&store, &mut routes, &selection)?;
    for warning in &report.warnings {
        writeln!(errors, "warning: {warning}")?;
    }
    writeln!(
        output,
        "restored={} removed_dead={} removed_orphans={} completed_operations={}",
        report.restored, report.removed_dead_leases, removed_orphans, report.completed_operations
    )?;
    Ok(())
}

fn reconcile_before_command(
    global: &crate::config::GlobalConfig,
    errors: &mut impl Write,
) -> crate::Result<()> {
    let store = state_store()?;
    let client = crate::caddy::Client::new(&global.caddy_admin)?;
    let config = client.fetch_config()?;
    let selection = available_servers(global, &config)?;
    let mut routes = crate::caddy::ManagedCaddyRoutes {
        client: &client,
        https_server: selection.https.as_deref(),
        http_server: selection.http.as_deref(),
        loopback_host: &global.caddy_loopback_host,
        client_ip_ranges: &global.caddy_client_ip_ranges,
    };
    let _operations = store.lock_operations()?;
    let report = reconcile_and_record(&store, &mut routes, &selection)?;
    for warning in report.warnings {
        writeln!(errors, "warning: {warning}")?;
    }
    Ok(())
}

fn reconcile_and_record(
    store: &crate::state::Store,
    routes: &mut impl RouteBackend,
    selection: &crate::caddy::ServerSelection,
) -> crate::Result<crate::reconcile::Report> {
    let synchronized_at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX);
    Ok(store.mutate(|registry| {
        let report = crate::reconcile::reconcile(registry, routes, crate::process::lease_liveness);
        registry.selected_servers.https = selection.https.clone();
        registry.selected_servers.http = selection.http.clone();
        registry.last_synchronized_at_unix_ms = Some(synchronized_at);
        Ok(report)
    })?)
}

fn state_store() -> crate::Result<crate::state::Store> {
    Ok(crate::state::Store::new(crate::state::state_path()?))
}

fn sha256_hex(input: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";

    let digest = Sha256::digest(input);
    let mut encoded = String::with_capacity(digest.len() * 2);
    for byte in digest {
        encoded.push(HEX[(byte >> 4) as usize] as char);
        encoded.push(HEX[(byte & 0x0f) as usize] as char);
    }
    encoded
}

fn select_servers(
    global: &crate::config::GlobalConfig,
    config: &serde_json::Value,
    need_https: bool,
    need_http: bool,
) -> crate::Result<crate::caddy::ServerSelection> {
    Ok(crate::caddy::discover_servers(
        config,
        crate::caddy::ServerOverrides {
            https: global.https_server.as_deref(),
            http: global.http_server.as_deref(),
        },
        need_https,
        need_http,
    )?)
}

fn available_servers(
    global: &crate::config::GlobalConfig,
    config: &serde_json::Value,
) -> crate::Result<crate::caddy::ServerSelection> {
    Ok(crate::caddy::discover_available_servers(
        config,
        crate::caddy::ServerOverrides {
            https: global.https_server.as_deref(),
            http: global.http_server.as_deref(),
        },
    )?)
}

fn expected_routes(registry: &crate::state::Registry) -> BTreeMap<uuid::Uuid, (&str, bool)> {
    registry
        .aliases
        .values()
        .map(|alias| (alias.id, (alias.hostname.as_str(), alias.tls)))
        .chain(
            registry
                .leases
                .values()
                .map(|lease| (lease.id, (lease.hostname.as_str(), lease.tls))),
        )
        .collect()
}

fn drift_messages(
    registry: &crate::state::Registry,
    inspection: &crate::caddy::ManagedInspection,
) -> Vec<String> {
    let expected = expected_routes(registry);
    let observed: BTreeMap<_, _> = inspection
        .routes
        .iter()
        .map(|route| (route.owner_id, (route.hostname.as_str(), route.tls)))
        .collect();
    let mut messages = Vec::new();
    for (owner, route) in &expected {
        if observed.get(owner) != Some(route) {
            messages.push(format!(
                "route {} is missing or differs from the registry",
                route.0
            ));
        }
    }
    for (owner, route) in observed {
        if !expected.contains_key(&owner) {
            messages.push(format!("route {} has no registry owner", route.0));
        }
    }
    messages
}

fn with_caddy_routes<T>(
    global: &crate::config::GlobalConfig,
    need_https: bool,
    need_http: bool,
    operation: impl FnOnce(&mut crate::caddy::ManagedCaddyRoutes<'_>) -> crate::Result<T>,
) -> crate::Result<T> {
    let client = crate::caddy::Client::new(&global.caddy_admin)?;
    let config = client.fetch_config()?;
    let selection = select_servers(global, &config, need_https, need_http)?;
    let mut routes = crate::caddy::ManagedCaddyRoutes {
        client: &client,
        https_server: selection.https.as_deref(),
        http_server: selection.http.as_deref(),
        loopback_host: &global.caddy_loopback_host,
        client_ip_ranges: &global.caddy_client_ip_ranges,
    };
    operation(&mut routes)
}

fn parse_from(arguments: impl IntoIterator<Item = OsString>) -> Result<Cli, clap::Error> {
    Cli::try_parse_from(normalize_shortcuts(arguments))
}

fn normalize_shortcuts(arguments: impl IntoIterator<Item = OsString>) -> Vec<OsString> {
    let mut arguments: Vec<_> = arguments.into_iter().collect();

    if !arguments.get(1).is_some_and(|value| value == "config")
        && let Some(index) = arguments
            .iter()
            .position(|value| value == "--caddy-socket")
            .filter(|index| *index > 1 && *index + 1 < arguments.len())
    {
        let option = arguments.remove(index);
        let value = arguments.remove(index);
        arguments.splice(1..1, [option, value]);
    }

    if arguments.get(2).is_some_and(|value| value == "run")
        && arguments.get(1).is_some_and(|value| !is_command(value))
    {
        let name = arguments.remove(1);
        arguments.splice(2..2, [OsString::from("--name"), name]);
    }

    if arguments.get(1).is_some_and(|value| value == "alias") {
        match arguments.get(2).map(OsString::as_os_str) {
            Some(value) if value == OsStr::new("--remove") => {
                arguments[2] = OsString::from("remove");
            }
            Some(value) if !is_alias_command(value) && !looks_like_option(value) => {
                arguments.insert(2, OsString::from("set"));
            }
            _ => {}
        }
    }

    arguments
}

fn is_command(value: &OsStr) -> bool {
    [
        "run",
        "alias",
        "list",
        "status",
        "stop",
        "prune",
        "ca",
        "config",
        "completions",
        "update",
    ]
    .iter()
    .any(|command| value == OsStr::new(command))
}

fn is_alias_command(value: &OsStr) -> bool {
    ["set", "remove", "list"]
        .iter()
        .any(|command| value == OsStr::new(command))
}

fn looks_like_option(value: &OsStr) -> bool {
    value.as_encoded_bytes().starts_with(b"-")
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;
    use std::io::{self, Write};
    use std::os::unix::ffi::{OsStrExt, OsStringExt};

    use clap::error::ErrorKind;

    use super::{
        AliasCommand, CaCommand, Command, CompletionShell, CompletionsArgs, ConfigCommand,
        ConfigKey, ConfigSetArgs, UpdateArgs, completions_command, drift_messages, parse_from,
        set_config_value, sha256_hex, write_certificate, write_registry_list,
    };
    use crate::state::{Alias, Lease, LeaseState, Registry, Scheme};
    use uuid::Uuid;

    #[test]
    fn sha256_hex_uses_lowercase_fixed_width_encoding() {
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn parses_name_before_run_and_all_run_options() {
        let cli = parse(&[
            "api",
            "run",
            "--no-tls",
            "--app-port",
            "5173",
            "--strict-port",
            "--force",
            "--config",
            "custom.toml",
            "--local",
            "--readiness-warn-after",
            "12",
            "--",
            "bun",
            "dev",
        ]);

        let Command::Run(run) = cli.command else {
            panic!("expected run command");
        };
        assert_eq!(run.name.as_deref(), Some("api"));
        assert!(run.no_tls);
        assert_eq!(run.app_port, Some(5173));
        assert!(run.strict_port);
        assert!(run.force);
        assert_eq!(
            run.config.as_deref(),
            Some(std::path::Path::new("custom.toml"))
        );
        assert!(run.local);
        assert_eq!(run.readiness_warn_after, Some(12));
        assert_eq!(run.command, ["bun", "dev"]);
    }

    #[test]
    fn parses_run_with_name_option() {
        let cli = parse(&["run", "--name", "api", "--", "server", "--flag"]);
        let Command::Run(run) = cli.command else {
            panic!("expected run command");
        };
        assert_eq!(run.name.as_deref(), Some("api"));
        assert_eq!(run.command, ["server", "--flag"]);
    }

    #[test]
    fn local_requires_an_explicit_project_configuration() {
        assert!(try_parse(&["run", "--local", "--", "server"]).is_err());
        assert!(try_parse(&["run", "--config", "custom.toml", "--local"]).is_ok());
    }

    #[test]
    fn preserves_non_utf8_child_arguments() {
        let argument = OsString::from_vec(vec![b'a', 0x80, b'b']);
        let cli = parse_from([
            OsString::from("nook"),
            OsString::from("run"),
            OsString::from("--"),
            OsString::from("server"),
            argument.clone(),
        ])
        .expect("run should parse");
        let Command::Run(run) = cli.command else {
            panic!("expected run command");
        };
        assert_eq!(run.command[1].as_bytes(), argument.as_bytes());
    }

    #[test]
    fn parses_canonical_and_short_alias_forms() {
        for arguments in [
            &["alias", "set", "api", "3000"][..],
            &["alias", "api", "3000"][..],
        ] {
            let cli = parse(arguments);
            let Command::Alias(alias) = cli.command else {
                panic!("expected alias command");
            };
            let AliasCommand::Set(set) = alias.command else {
                panic!("expected alias set command");
            };
            assert_eq!(set.name, "api");
            assert_eq!(set.target, "3000");
        }

        for arguments in [
            &["alias", "remove", "api"][..],
            &["alias", "--remove", "api"][..],
        ] {
            let cli = parse(arguments);
            let Command::Alias(alias) = cli.command else {
                panic!("expected alias command");
            };
            assert!(matches!(alias.command, AliasCommand::Remove(_)));
        }
    }

    #[test]
    fn parses_operational_commands() {
        for arguments in [
            &["alias", "list"][..],
            &["list"][..],
            &["status"][..],
            &["stop", "api", "--force"][..],
            &["prune"][..],
            &["update"][..],
            &["update", "--check"][..],
            &["update", "--force"][..],
        ] {
            parse(arguments);
        }
    }

    #[test]
    fn parses_update_flags() {
        let cli = parse(&["update", "--check", "--force"]);
        assert!(matches!(
            cli.command,
            Command::Update(UpdateArgs {
                check: true,
                force: true
            })
        ));
    }

    #[test]
    fn parses_supported_completion_shells_and_rejects_others() {
        for (shell, expected) in [
            ("bash", CompletionShell::Bash),
            ("zsh", CompletionShell::Zsh),
        ] {
            let cli = parse(&["completions", shell]);
            let Command::Completions(arguments) = cli.command else {
                panic!("expected completions command");
            };
            assert_eq!(arguments.shell, expected);
        }

        assert_eq!(
            try_parse(&["completions", "fish"])
                .expect_err("unsupported shells should be rejected")
                .kind(),
            ErrorKind::InvalidValue
        );
    }

    #[test]
    fn completion_generation_propagates_output_errors() {
        struct FailingWriter;

        impl Write for FailingWriter {
            fn write(&mut self, _buffer: &[u8]) -> io::Result<usize> {
                Err(io::Error::other("completion output failed"))
            }

            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }

        let error = completions_command(
            CompletionsArgs {
                shell: CompletionShell::Bash,
            },
            &mut FailingWriter,
        )
        .expect_err("completion write errors should be returned");
        assert_eq!(error.kind(), io::ErrorKind::Other);
        assert_eq!(error.to_string(), "completion output failed");
    }

    #[test]
    fn parses_ca_export_and_writes_without_unsafe_overwrite() {
        let cli = parse(&["ca", "export", "/tmp/caddy-ca.pem", "--force"]);
        assert!(matches!(
            cli.command,
            Command::Ca(super::CaArgs {
                command: CaCommand::Export(super::CaExportArgs { force: true, .. })
            })
        ));

        let directory = std::env::temp_dir().join(format!("nook-ca-export-{}", Uuid::new_v4()));
        std::fs::create_dir(&directory).unwrap();
        let path = directory.join("ca.pem");
        write_certificate(&path, b"certificate", false).unwrap();
        assert!(write_certificate(&path, b"other", false).is_err());
        write_certificate(&path, b"replacement", true).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"replacement");
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn parses_global_configuration_commands() {
        let init = parse(&[
            "config",
            "init",
            "--caddy-socket",
            "/run/caddy/admin.socket",
            "--force",
        ]);
        assert!(matches!(
            init.command,
            Command::Config(super::ConfigArgs {
                command: ConfigCommand::Init(super::ConfigInitArgs {
                    caddy_socket: Some(ref socket),
                    force: true
                })
            }) if socket == "/run/caddy/admin.socket"
        ));
        assert!(matches!(
            parse(&["config", "show"]).command,
            Command::Config(super::ConfigArgs {
                command: ConfigCommand::Show
            })
        ));
        assert!(matches!(
            parse(&["config", "path"]).command,
            Command::Config(super::ConfigArgs {
                command: ConfigCommand::Path
            })
        ));

        let set = parse(&["config", "set", "caddy-admin", "unix:///run/caddy.sock"]);
        let Command::Config(super::ConfigArgs {
            command: ConfigCommand::Set(set),
        }) = set.command
        else {
            panic!("expected config set");
        };
        assert!(matches!(set.key, ConfigKey::CaddyAdmin));
    }

    #[test]
    fn configuration_values_are_typed_and_validated() {
        let mut config = crate::config::GlobalConfig::default();
        set_config_value(
            &mut config,
            ConfigSetArgs {
                key: ConfigKey::CaddyClientIpRanges,
                value: "127.0.0.1/32, ::1".into(),
            },
        )
        .unwrap();
        assert_eq!(config.caddy_client_ip_ranges, ["127.0.0.1/32", "::1"]);
        assert!(
            set_config_value(
                &mut config,
                ConfigSetArgs {
                    key: ConfigKey::RunBindAddress,
                    value: "not-an-ip".into(),
                }
            )
            .is_err()
        );
    }

    #[test]
    fn caddy_socket_is_accepted_around_operational_commands() {
        for arguments in [
            &["--caddy-socket", "/run/caddy/admin.socket", "status"][..],
            &["status", "--caddy-socket", "/run/caddy/admin.socket"][..],
        ] {
            let cli = parse(arguments);
            assert_eq!(cli.caddy_socket.as_deref(), Some("/run/caddy/admin.socket"));
            assert!(matches!(cli.command, Command::Status));
        }

        assert!(try_parse(&["config", "show", "--caddy-socket", "/tmp/caddy.sock"]).is_err());
    }

    #[test]
    fn accepts_a_run_without_cli_command_for_project_configuration() {
        let cli = parse(&["run"]);
        let Command::Run(run) = cli.command else {
            panic!("expected run command");
        };
        assert!(run.command.is_empty());
    }

    #[test]
    fn rejects_strict_port_without_a_requested_port() {
        let error = try_parse(&["run", "--strict-port", "--", "server"])
            .expect_err("arguments should be rejected");
        assert_eq!(error.kind(), ErrorKind::MissingRequiredArgument);
    }

    #[test]
    fn list_output_distinguishes_starting_ready_and_persistent_aliases() {
        let mut registry = Registry::empty();
        for (hostname, state) in [
            ("starting.localhost", LeaseState::Starting),
            ("ready.localhost", LeaseState::Ready),
        ] {
            let id = Uuid::new_v4();
            registry.leases.insert(
                id,
                Lease {
                    id,
                    hostname: hostname.into(),
                    target: "http://127.0.0.1:3000".into(),
                    scheme: Scheme::Http,
                    tls: true,
                    pid: 1,
                    pgid: 1,
                    process_start_time_ticks: 1,
                    state,
                },
            );
        }
        let alias = Alias {
            id: Uuid::new_v4(),
            hostname: "alias.localhost".into(),
            target: "http://127.0.0.1:4000".into(),
            scheme: Scheme::Http,
            tls: true,
            preserve_host: false,
        };
        registry.aliases.insert(alias.hostname.clone(), alias);
        let mut output = Vec::new();
        write_registry_list(&registry, &mut output).unwrap();
        let output = String::from_utf8(output).unwrap();
        assert!(output.contains("run\tstarting\tstarting.localhost"));
        assert!(output.contains("run\tready\tready.localhost"));
        assert!(output.contains("alias\tpersistent\talias.localhost"));
    }

    #[test]
    fn status_drift_identifies_missing_and_orphaned_routes() {
        let mut registry = Registry::empty();
        let alias = Alias {
            id: Uuid::new_v4(),
            hostname: "missing.localhost".into(),
            target: "http://127.0.0.1:4000".into(),
            scheme: Scheme::Http,
            tls: true,
            preserve_host: false,
        };
        registry.aliases.insert(alias.hostname.clone(), alias);
        let inspection = crate::caddy::ManagedInspection {
            routes: vec![crate::caddy::ManagedObservation {
                owner_id: Uuid::new_v4(),
                hostname: "orphan.localhost".into(),
                tls: true,
            }],
            ..crate::caddy::ManagedInspection::default()
        };
        let messages = drift_messages(&registry, &inspection);
        assert!(
            messages
                .iter()
                .any(|message| message.contains("missing.localhost"))
        );
        assert!(
            messages
                .iter()
                .any(|message| message.contains("orphan.localhost"))
        );
    }

    fn parse(arguments: &[&str]) -> super::Cli {
        try_parse(arguments).expect("arguments should parse")
    }

    fn try_parse(arguments: &[&str]) -> Result<super::Cli, clap::Error> {
        parse_from(
            std::iter::once(OsString::from("nook")).chain(arguments.iter().map(OsString::from)),
        )
    }
}
