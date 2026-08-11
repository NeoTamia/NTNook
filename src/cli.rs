//! Command-line parsing, terminal output, and exit-code policy.

use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::io::{self, Write};
use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};

use crate::reconcile::RouteBackend;

const ACCEPTED: &str = "Command accepted; operational behavior is not implemented yet.\n";

#[derive(Debug, Parser)]
#[command(
    name = "nook",
    version,
    about = "Expose local services through stable *.localhost domains",
    arg_required_else_help = true
)]
pub(crate) struct Cli {
    #[command(subcommand)]
    pub(crate) command: Command,
}

#[derive(Debug, Subcommand)]
pub(crate) enum Command {
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
    pub(crate) name: String,
    pub(crate) target: String,
    #[arg(long)]
    pub(crate) no_tls: bool,
    #[arg(long)]
    pub(crate) preserve_host: bool,
    #[arg(long)]
    pub(crate) force: bool,
}

#[derive(Debug, Args)]
pub(crate) struct AliasRemoveArgs {
    pub(crate) name: String,
}

#[derive(Debug, Args)]
pub(crate) struct StopArgs {
    name: String,
    #[arg(long)]
    force: bool,
}

pub(crate) fn run() -> crate::Result<()> {
    let cli = parse_from(std::env::args_os())?;
    let stdout = io::stdout();
    let stderr = io::stderr();
    execute(cli, &mut stdout.lock(), &mut stderr.lock())
}

fn execute(cli: Cli, output: &mut impl Write, errors: &mut impl Write) -> crate::Result<()> {
    match cli.command {
        Command::Alias(AliasArgs {
            command: AliasCommand::Set(arguments),
        }) => set_alias_command(arguments, output, errors),
        Command::Alias(AliasArgs {
            command: AliasCommand::Remove(arguments),
        }) => remove_alias_command(arguments, output, errors),
        Command::Alias(AliasArgs {
            command: AliasCommand::List,
        }) => list_alias_command(output),
        Command::List => list_command(output),
        Command::Status => status_command(output, errors),
        Command::Prune => prune_command(output, errors),
        Command::Run(arguments) => {
            let _run_config = crate::config::resolve_run(&arguments, &std::env::current_dir()?)?;
            output.write_all(ACCEPTED.as_bytes())?;
            Ok(())
        }
        _ => {
            output.write_all(ACCEPTED.as_bytes())?;
            Ok(())
        }
    }
}

fn set_alias_command(
    arguments: AliasSetArgs,
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
    let global = crate::config::load_global()?;
    with_caddy_routes(&global, request.tls, !request.tls, |routes| {
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
    let global = crate::config::load_global()?;
    with_caddy_routes(&global, alias.tls, !alias.tls, |routes| {
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

fn status_command(output: &mut impl Write, errors: &mut impl Write) -> crate::Result<()> {
    let registry = state_store()?.load()?;
    let global = crate::config::load_global()?;
    let client = crate::caddy::Client::new(&global.caddy_admin)?;
    let config = client.fetch_config()?;
    let selection = available_servers(&global, &config)?;
    let inspection = crate::caddy::inspect_managed(&config, &selection)?;
    writeln!(output, "caddy\tok")?;
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
    Ok(())
}

fn prune_command(output: &mut impl Write, errors: &mut impl Write) -> crate::Result<()> {
    let store = state_store()?;
    let registry = store.load()?;
    let global = crate::config::load_global()?;
    let client = crate::caddy::Client::new(&global.caddy_admin)?;
    let config = client.fetch_config()?;
    let selection = available_servers(&global, &config)?;
    let inspection = crate::caddy::inspect_managed(&config, &selection)?;
    let mut routes = crate::caddy::ManagedCaddyRoutes {
        client: &client,
        https_server: selection.https.as_deref(),
        http_server: selection.http.as_deref(),
    };
    let _operations = store.lock_operations()?;
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
    let report =
        crate::reconcile::reconcile_store(&store, &mut routes, crate::process::lease_liveness)?;
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

fn state_store() -> crate::Result<crate::state::Store> {
    Ok(crate::state::Store::new(crate::state::state_path()?))
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
    };
    operation(&mut routes)
}

fn parse_from(arguments: impl IntoIterator<Item = OsString>) -> Result<Cli, clap::Error> {
    Cli::try_parse_from(normalize_shortcuts(arguments))
}

fn normalize_shortcuts(arguments: impl IntoIterator<Item = OsString>) -> Vec<OsString> {
    let mut arguments: Vec<_> = arguments.into_iter().collect();

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
    ["run", "alias", "list", "status", "stop", "prune"]
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
    use std::os::unix::ffi::{OsStrExt, OsStringExt};

    use clap::error::ErrorKind;

    use super::{AliasCommand, Command, parse_from, write_registry_list};
    use crate::state::{Alias, Lease, LeaseState, Registry, Scheme};
    use uuid::Uuid;

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
        ] {
            parse(arguments);
        }
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

    fn parse(arguments: &[&str]) -> super::Cli {
        try_parse(arguments).expect("arguments should parse")
    }

    fn try_parse(arguments: &[&str]) -> Result<super::Cli, clap::Error> {
        parse_from(
            std::iter::once(OsString::from("nook")).chain(arguments.iter().map(OsString::from)),
        )
    }
}
