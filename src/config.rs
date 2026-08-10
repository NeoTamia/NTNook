//! Global/project configuration and canonical application-name resolution.

use std::env;
use std::ffi::OsString;
use std::fmt;
use std::fs;
use std::io;
use std::os::unix::ffi::OsStringExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde::Deserialize;

use crate::cli::RunArgs;

const FORMAT_VERSION: u32 = 1;
const DEFAULT_CADDY_ADMIN: &str = "http://127.0.0.1:2019";
const DEFAULT_READINESS_WARN_AFTER_SECONDS: u64 = 30;

#[derive(Debug)]
pub(crate) enum Error {
    MissingHome,
    Read {
        path: PathBuf,
        source: io::Error,
    },
    Parse {
        path: PathBuf,
        source: toml::de::Error,
    },
    UnsupportedVersion {
        path: PathBuf,
        version: u32,
    },
    MissingCommand,
    MissingName,
    InvalidName {
        name: String,
        reason: &'static str,
    },
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingHome => {
                write!(formatter, "cannot locate the home configuration directory")
            }
            Self::Read { path, source } => {
                write!(formatter, "cannot read {}: {source}", path.display())
            }
            Self::Parse { path, source } => write!(
                formatter,
                "invalid configuration {}: {source}",
                path.display()
            ),
            Self::UnsupportedVersion { path, version } => write!(
                formatter,
                "unsupported format_version {version} in {}; expected {FORMAT_VERSION}",
                path.display()
            ),
            Self::MissingCommand => write!(
                formatter,
                "a child command is required after `--` or in nook.toml"
            ),
            Self::MissingName => write!(formatter, "cannot infer an application name"),
            Self::InvalidName { name, reason } => {
                write!(formatter, "invalid application name `{name}`: {reason}")
            }
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Read { source, .. } => Some(source),
            Self::Parse { source, .. } => Some(source),
            _ => None,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ProjectConfig {
    format_version: u32,
    name: Option<String>,
    command: Option<Vec<String>>,
    tls: Option<bool>,
    app_port: Option<u16>,
    strict_port: Option<bool>,
    readiness_warn_after_seconds: Option<u64>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
#[allow(dead_code)]
pub(crate) struct GlobalConfig {
    format_version: u32,
    pub(crate) caddy_admin: String,
    pub(crate) https_server: Option<String>,
    pub(crate) http_server: Option<String>,
}

impl Default for GlobalConfig {
    fn default() -> Self {
        Self {
            format_version: FORMAT_VERSION,
            caddy_admin: DEFAULT_CADDY_ADMIN.to_owned(),
            https_server: None,
            http_server: None,
        }
    }
}

#[derive(Debug)]
#[allow(dead_code)]
pub(crate) struct ResolvedRunConfig {
    pub(crate) hostname: String,
    pub(crate) command: Vec<OsString>,
    pub(crate) tls: bool,
    pub(crate) app_port: Option<u16>,
    pub(crate) strict_port: bool,
    pub(crate) readiness_warn_after_seconds: u64,
}

pub(crate) fn load_global() -> Result<GlobalConfig, Error> {
    let path = global_config_path_with(|key| env::var_os(key))?;
    let Some(contents) = read_optional(&path)? else {
        return Ok(GlobalConfig::default());
    };
    let config: GlobalConfig = parse(&path, &contents)?;
    validate_version(&path, config.format_version)?;
    Ok(config)
}

pub(crate) fn resolve_run(
    arguments: &RunArgs,
    current_directory: &Path,
) -> Result<ResolvedRunConfig, Error> {
    let project = load_project(arguments.config.as_deref(), current_directory)?;
    let git_root = find_git_root(current_directory);
    merge_run(arguments, project, git_root.as_deref(), current_directory)
}

fn load_project(
    explicit_path: Option<&Path>,
    current_directory: &Path,
) -> Result<Option<ProjectConfig>, Error> {
    let path = explicit_path
        .map(Path::to_path_buf)
        .unwrap_or_else(|| current_directory.join("nook.toml"));
    let contents = if explicit_path.is_some() {
        Some(read_required(&path)?)
    } else {
        read_optional(&path)?
    };
    let Some(contents) = contents else {
        return Ok(None);
    };
    let config: ProjectConfig = parse(&path, &contents)?;
    validate_version(&path, config.format_version)?;
    Ok(Some(config))
}

fn merge_run(
    arguments: &RunArgs,
    project: Option<ProjectConfig>,
    git_root: Option<&Path>,
    current_directory: &Path,
) -> Result<ResolvedRunConfig, Error> {
    let project = project.unwrap_or(ProjectConfig {
        format_version: FORMAT_VERSION,
        name: None,
        command: None,
        tls: None,
        app_port: None,
        strict_port: None,
        readiness_warn_after_seconds: None,
    });
    let hostname = resolve_hostname(
        arguments.name.as_deref(),
        project.name.as_deref(),
        git_root,
        current_directory,
    )?;
    let command = if arguments.command.is_empty() {
        project
            .command
            .map(|values| values.into_iter().map(OsString::from).collect())
            .ok_or(Error::MissingCommand)?
    } else {
        arguments.command.clone()
    };
    Ok(ResolvedRunConfig {
        hostname,
        command,
        tls: if arguments.no_tls {
            false
        } else {
            project.tls.unwrap_or(true)
        },
        app_port: arguments.app_port.or(project.app_port),
        strict_port: arguments.strict_port || project.strict_port.unwrap_or(false),
        readiness_warn_after_seconds: arguments
            .readiness_warn_after
            .or(project.readiness_warn_after_seconds)
            .unwrap_or(DEFAULT_READINESS_WARN_AFTER_SECONDS),
    })
}

fn resolve_hostname(
    cli_name: Option<&str>,
    project_name: Option<&str>,
    git_root: Option<&Path>,
    current_directory: &Path,
) -> Result<String, Error> {
    let inferred = git_root
        .and_then(Path::file_name)
        .or_else(|| current_directory.file_name())
        .and_then(|name| name.to_str());
    normalize_hostname(
        cli_name
            .or(project_name)
            .or(inferred)
            .ok_or(Error::MissingName)?,
    )
}

fn normalize_hostname(name: &str) -> Result<String, Error> {
    if !name.is_ascii() {
        return invalid_name(name, "only ASCII DNS labels are supported");
    }
    let name = name.to_ascii_lowercase();
    if name == "localhost" {
        return invalid_name(&name, "an application label is required before .localhost");
    }
    if name.contains(".localhost") && !name.ends_with(".localhost") {
        return invalid_name(&name, ".localhost may only appear as the final suffix");
    }
    let labels = name.strip_suffix(".localhost").unwrap_or(&name);
    if labels.is_empty() {
        return invalid_name(&name, "an application label is required");
    }
    for label in labels.split('.') {
        if label.is_empty() {
            return invalid_name(&name, "DNS labels cannot be empty");
        }
        if label.len() > 63 {
            return invalid_name(&name, "DNS labels cannot exceed 63 bytes");
        }
        if !label
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        {
            return invalid_name(
                &name,
                "DNS labels may contain only letters, digits, and hyphens",
            );
        }
        if label.starts_with('-') || label.ends_with('-') {
            return invalid_name(
                &name,
                "DNS labels must start and end with a letter or digit",
            );
        }
    }
    let hostname = if name.ends_with(".localhost") {
        name
    } else {
        format!("{name}.localhost")
    };
    if hostname.len() > 253 {
        return invalid_name(&hostname, "the hostname cannot exceed 253 bytes");
    }
    Ok(hostname)
}

fn invalid_name<T>(name: &str, reason: &'static str) -> Result<T, Error> {
    Err(Error::InvalidName {
        name: name.to_owned(),
        reason,
    })
}

fn find_git_root(current_directory: &Path) -> Option<PathBuf> {
    let output = Command::new("git")
        .arg("-C")
        .arg(current_directory)
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let mut path = output.stdout;
    while path.last().is_some_and(u8::is_ascii_whitespace) {
        path.pop();
    }
    (!path.is_empty()).then(|| PathBuf::from(OsString::from_vec(path)))
}

fn global_config_path_with(get: impl Fn(&str) -> Option<OsString>) -> Result<PathBuf, Error> {
    if let Some(directory) = get("XDG_CONFIG_HOME").filter(|value| !value.is_empty()) {
        return Ok(PathBuf::from(directory).join("nook/config.toml"));
    }
    get("HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .map(|home| home.join(".config/nook/config.toml"))
        .ok_or(Error::MissingHome)
}

fn read_required(path: &Path) -> Result<String, Error> {
    fs::read_to_string(path).map_err(|source| Error::Read {
        path: path.to_path_buf(),
        source,
    })
}

fn read_optional(path: &Path) -> Result<Option<String>, Error> {
    match fs::read_to_string(path) {
        Ok(contents) => Ok(Some(contents)),
        Err(source) if source.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(source) => Err(Error::Read {
            path: path.to_path_buf(),
            source,
        }),
    }
}

fn parse<T: for<'de> Deserialize<'de>>(path: &Path, contents: &str) -> Result<T, Error> {
    toml::from_str(contents).map_err(|source| Error::Parse {
        path: path.to_path_buf(),
        source,
    })
}

fn validate_version(path: &Path, version: u32) -> Result<(), Error> {
    if version == FORMAT_VERSION {
        Ok(())
    } else {
        Err(Error::UnsupportedVersion {
            path: path.to_path_buf(),
            version,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{
        Error, GlobalConfig, ProjectConfig, global_config_path_with, merge_run, normalize_hostname,
        resolve_hostname,
    };
    use crate::cli::{Cli, Command};
    use clap::Parser;
    use std::ffi::OsString;
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn global_path_prefers_xdg_and_falls_back_to_home() {
        let xdg = global_config_path_with(|key| match key {
            "XDG_CONFIG_HOME" => Some(OsString::from("/xdg")),
            "HOME" => Some(OsString::from("/home/user")),
            _ => None,
        })
        .unwrap();
        assert_eq!(xdg, Path::new("/xdg/nook/config.toml"));
        let home =
            global_config_path_with(|key| (key == "HOME").then(|| OsString::from("/home/user")))
                .unwrap();
        assert_eq!(home, Path::new("/home/user/.config/nook/config.toml"));
    }

    #[test]
    fn global_defaults_include_admin_api_and_empty_overrides() {
        let config = GlobalConfig::default();
        assert_eq!(config.caddy_admin, "http://127.0.0.1:2019");
        assert!(config.https_server.is_none() && config.http_server.is_none());
    }

    #[test]
    fn cli_values_override_project_values() {
        let run = run_args(&[
            "run",
            "--name",
            "cli",
            "--no-tls",
            "--app-port",
            "9000",
            "--strict-port",
            "--readiness-warn-after",
            "5",
            "--",
            "cli-command",
        ]);
        let resolved = merge_run(
            &run,
            Some(project()),
            Some(Path::new("/git")),
            Path::new("/cwd"),
        )
        .unwrap();
        assert_eq!(resolved.hostname, "cli.localhost");
        assert_eq!(resolved.command, ["cli-command"]);
        assert!(!resolved.tls);
        assert_eq!(resolved.app_port, Some(9000));
        assert!(resolved.strict_port);
        assert_eq!(resolved.readiness_warn_after_seconds, 5);
    }

    #[test]
    fn project_values_and_defaults_are_used_without_cli_values() {
        let resolved = merge_run(
            &run_args(&["run"]),
            Some(project()),
            Some(Path::new("/git")),
            Path::new("/cwd"),
        )
        .unwrap();
        assert_eq!(resolved.hostname, "project.localhost");
        assert_eq!(resolved.command, ["project-command"]);
        assert!(resolved.tls);
        assert_eq!(resolved.app_port, Some(8000));
        assert!(!resolved.strict_port);
        assert_eq!(resolved.readiness_warn_after_seconds, 20);
    }

    #[test]
    fn command_is_required_after_merge() {
        assert!(matches!(
            merge_run(
                &run_args(&["run"]),
                None,
                Some(Path::new("/git")),
                Path::new("/cwd")
            ),
            Err(Error::MissingCommand)
        ));
    }

    #[test]
    fn explicit_missing_file_and_unknown_version_fail() {
        let directory = temporary_directory();
        let missing = directory.join("missing.toml");
        let run = run_args(&["run", "--config", missing.to_str().unwrap(), "--", "server"]);
        assert!(matches!(
            super::resolve_run(&run, &directory),
            Err(Error::Read { .. })
        ));
        let invalid = directory.join("invalid.toml");
        fs::write(&invalid, "format_version = 2\ncommand = [\"server\"]\n").unwrap();
        let run = run_args(&["run", "--config", invalid.to_str().unwrap()]);
        assert!(matches!(
            super::resolve_run(&run, &directory),
            Err(Error::UnsupportedVersion { version: 2, .. })
        ));
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn command_must_be_a_toml_array() {
        assert!(
            toml::from_str::<ProjectConfig>("format_version = 1\ncommand = \"bun dev\"\n").is_err()
        );
    }

    #[test]
    fn name_priority_is_cli_then_project_then_git_then_current_directory() {
        assert_eq!(
            resolve_hostname(
                Some("cli"),
                Some("project"),
                Some(Path::new("/roots/git")),
                Path::new("/work/current")
            )
            .unwrap(),
            "cli.localhost"
        );
        assert_eq!(
            resolve_hostname(
                None,
                Some("project"),
                Some(Path::new("/roots/git")),
                Path::new("/work/current")
            )
            .unwrap(),
            "project.localhost"
        );
        assert_eq!(
            resolve_hostname(
                None,
                None,
                Some(Path::new("/roots/git")),
                Path::new("/work/current")
            )
            .unwrap(),
            "git.localhost"
        );
        assert_eq!(
            resolve_hostname(None, None, None, Path::new("/work/current")).unwrap(),
            "current.localhost"
        );
    }

    #[test]
    fn normalizes_valid_names() {
        assert_eq!(normalize_hostname("API").unwrap(), "api.localhost");
        assert_eq!(
            normalize_hostname("api.neotamia").unwrap(),
            "api.neotamia.localhost"
        );
        assert_eq!(
            normalize_hostname("Api.Localhost").unwrap(),
            "api.localhost"
        );
    }

    #[test]
    fn rejects_invalid_dns_labels() {
        for name in [
            "équipe",
            "api_name",
            "-api",
            "api-",
            "api..dev",
            "localhost",
            "api.localhost.dev",
        ] {
            assert!(
                matches!(normalize_hostname(name), Err(Error::InvalidName { .. })),
                "{name} should fail"
            );
        }
        assert!(matches!(
            normalize_hostname(&"a".repeat(64)),
            Err(Error::InvalidName { .. })
        ));
    }

    fn project() -> ProjectConfig {
        ProjectConfig {
            format_version: 1,
            name: Some("project".into()),
            command: Some(vec!["project-command".into()]),
            tls: Some(true),
            app_port: Some(8000),
            strict_port: Some(false),
            readiness_warn_after_seconds: Some(20),
        }
    }

    fn run_args(arguments: &[&str]) -> crate::cli::RunArgs {
        let cli =
            Cli::try_parse_from(std::iter::once("nook").chain(arguments.iter().copied())).unwrap();
        let Command::Run(run) = cli.command else {
            panic!("expected run")
        };
        run
    }

    fn temporary_directory() -> PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path =
            std::env::temp_dir().join(format!("nook-config-{}-{unique}", std::process::id()));
        fs::create_dir(&path).unwrap();
        path
    }
}
