//! Command-line parsing, terminal output, and exit-code policy.

use std::ffi::{OsStr, OsString};
use std::io::{self, Write};
use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};

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
    force: bool,
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
    name: String,
    pub(crate) target: String,
    #[arg(long)]
    no_tls: bool,
    #[arg(long)]
    preserve_host: bool,
    #[arg(long)]
    force: bool,
}

#[derive(Debug, Args)]
pub(crate) struct AliasRemoveArgs {
    name: String,
}

#[derive(Debug, Args)]
pub(crate) struct StopArgs {
    name: String,
    #[arg(long)]
    force: bool,
}

pub(crate) fn run() -> crate::Result<()> {
    let cli = parse_from(std::env::args_os())?;
    let _global_config = crate::config::load_global()?;
    if let Command::Run(arguments) = &cli.command {
        let _run_config = crate::config::resolve_run(arguments, &std::env::current_dir()?)?;
    }
    if let Command::Alias(AliasArgs {
        command: AliasCommand::Set(arguments),
    }) = &cli.command
    {
        let _upstream = crate::caddy::normalize_upstream(&arguments.target)?;
    }
    let stdout = io::stdout();
    let mut output = stdout.lock();
    output.write_all(ACCEPTED.as_bytes())?;
    Ok(())
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

    use super::{AliasCommand, Command, parse_from};

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

    fn parse(arguments: &[&str]) -> super::Cli {
        try_parse(arguments).expect("arguments should parse")
    }

    fn try_parse(arguments: &[&str]) -> Result<super::Cli, clap::Error> {
        parse_from(
            std::iter::once(OsString::from("nook")).chain(arguments.iter().map(OsString::from)),
        )
    }
}
