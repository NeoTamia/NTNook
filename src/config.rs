//! Global/project configuration and canonical application-name resolution.

use std::env;
use std::ffi::OsString;
use std::fmt;
use std::fs;
use std::io;
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde::{Deserialize, Serialize};

use crate::cli::RunArgs;

const FORMAT_VERSION: u32 = 1;
const DEFAULT_CADDY_ADMIN: &str = "http://127.0.0.1:2019";
const DEFAULT_READINESS_WARN_AFTER_SECONDS: u64 = 30;

fn default_run_bind_address() -> IpAddr {
    IpAddr::V4(std::net::Ipv4Addr::LOCALHOST)
}

fn default_caddy_loopback_host() -> String {
    "127.0.0.1".to_owned()
}

fn default_caddy_client_ip_ranges() -> Vec<String> {
    vec!["127.0.0.0/8".to_owned(), "::1".to_owned()]
}

#[derive(Debug)]
pub(crate) enum Error {
    MissingHome,
    Read {
        path: PathBuf,
        source: io::Error,
    },
    Write {
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
    InvalidGlobal {
        field: &'static str,
        reason: String,
    },
    Serialize(toml::ser::Error),
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
            Self::Write { path, source } => {
                write!(formatter, "cannot write {}: {source}", path.display())
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
            Self::InvalidGlobal { field, reason } => {
                write!(
                    formatter,
                    "invalid global configuration `{field}`: {reason}"
                )
            }
            Self::Serialize(error) => {
                write!(formatter, "cannot serialize global configuration: {error}")
            }
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Read { source, .. } | Self::Write { source, .. } => Some(source),
            Self::Parse { source, .. } => Some(source),
            Self::Serialize(error) => Some(error),
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
    run_bind_address: Option<IpAddr>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[allow(dead_code)]
pub(crate) struct GlobalConfig {
    format_version: u32,
    pub(crate) caddy_admin: String,
    pub(crate) https_server: Option<String>,
    pub(crate) http_server: Option<String>,
    #[serde(default = "default_run_bind_address")]
    pub(crate) run_bind_address: IpAddr,
    #[serde(default = "default_caddy_loopback_host")]
    pub(crate) caddy_loopback_host: String,
    #[serde(default = "default_caddy_client_ip_ranges")]
    pub(crate) caddy_client_ip_ranges: Vec<String>,
}

impl GlobalConfig {
    pub(crate) fn set_caddy_admin(&mut self, value: String) {
        self.caddy_admin = value;
    }

    pub(crate) fn set_https_server(&mut self, value: Option<String>) {
        self.https_server = value;
    }

    pub(crate) fn set_http_server(&mut self, value: Option<String>) {
        self.http_server = value;
    }

    pub(crate) fn set_run_bind_address(&mut self, value: IpAddr) {
        self.run_bind_address = value;
    }

    pub(crate) fn set_caddy_loopback_host(&mut self, value: String) {
        self.caddy_loopback_host = value;
    }

    pub(crate) fn set_caddy_client_ip_ranges(&mut self, value: Vec<String>) {
        self.caddy_client_ip_ranges = value;
    }
}

impl Default for GlobalConfig {
    fn default() -> Self {
        Self {
            format_version: FORMAT_VERSION,
            caddy_admin: DEFAULT_CADDY_ADMIN.to_owned(),
            https_server: None,
            http_server: None,
            run_bind_address: default_run_bind_address(),
            caddy_loopback_host: default_caddy_loopback_host(),
            caddy_client_ip_ranges: default_caddy_client_ip_ranges(),
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
    pub(crate) force: bool,
    pub(crate) readiness_warn_after_seconds: u64,
    pub(crate) bind_address: IpAddr,
    pub(crate) ignored_local_config: Option<PathBuf>,
}

pub(crate) fn load_global() -> Result<GlobalConfig, Error> {
    let path = global_config_path_with(|key| env::var_os(key))?;
    let Some(contents) = read_optional(&path)? else {
        return Ok(GlobalConfig::default());
    };
    let config: GlobalConfig = parse(&path, &contents)?;
    validate_version(&path, config.format_version)?;
    validate_global(&config)?;
    Ok(config)
}

pub(crate) fn global_config_path() -> Result<PathBuf, Error> {
    global_config_path_with(|key| env::var_os(key))
}

pub(crate) fn format_global(config: &GlobalConfig) -> Result<String, Error> {
    validate_global(config)?;
    toml::to_string_pretty(config).map_err(Error::Serialize)
}

pub(crate) fn write_global(config: &GlobalConfig, force: bool) -> Result<PathBuf, Error> {
    let path = global_config_path()?;
    write_global_at(&path, config, force)?;
    Ok(path)
}

fn write_global_at(path: &Path, config: &GlobalConfig, force: bool) -> Result<(), Error> {
    let contents = format_global(config)?;
    let parent = path.parent().ok_or_else(|| Error::Write {
        path: path.to_path_buf(),
        source: io::Error::new(
            io::ErrorKind::InvalidInput,
            "configuration path has no parent",
        ),
    })?;
    fs::create_dir_all(parent).map_err(|source| Error::Write {
        path: parent.to_path_buf(),
        source,
    })?;
    if let Ok(metadata) = fs::symlink_metadata(path) {
        if !force {
            return Err(Error::Write {
                path: path.to_path_buf(),
                source: io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    "configuration already exists; use --force to replace it",
                ),
            });
        }
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            return Err(Error::Write {
                path: path.to_path_buf(),
                source: io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "configuration path is not a regular file",
                ),
            });
        }
    }
    let temporary = parent.join(format!(".config.toml.{}.tmp", uuid::Uuid::new_v4()));
    let result = (|| {
        use std::io::Write;

        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o644);
        }
        let mut file = options.open(&temporary)?;
        file.write_all(contents.as_bytes())?;
        file.sync_all()?;
        if force {
            crate::platform::replace_file(&temporary, path)?;
        } else {
            fs::hard_link(&temporary, path)?;
            fs::remove_file(&temporary)?;
        }
        Ok::<(), io::Error>(())
    })();
    if temporary.exists() {
        let _ = fs::remove_file(&temporary);
    }
    result.map_err(|source| Error::Write {
        path: path.to_path_buf(),
        source,
    })
}

fn validate_global(config: &GlobalConfig) -> Result<(), Error> {
    let valid_ip = config.caddy_loopback_host.parse::<IpAddr>().is_ok();
    if !valid_ip
        && (config.caddy_loopback_host.is_empty()
            || config.caddy_loopback_host.contains(['/', ':'])
            || config.caddy_loopback_host.split('.').any(|label| {
                label.is_empty()
                    || label.starts_with('-')
                    || label.ends_with('-')
                    || !label
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
            }))
    {
        return Err(Error::InvalidGlobal {
            field: "caddy_loopback_host",
            reason: "expected an IP address or DNS hostname without scheme or port".into(),
        });
    }
    if config.caddy_client_ip_ranges.is_empty() {
        return Err(Error::InvalidGlobal {
            field: "caddy_client_ip_ranges",
            reason: "at least one IP address or CIDR is required".into(),
        });
    }
    for range in &config.caddy_client_ip_ranges {
        if range.parse::<ipnet::IpNet>().is_err() && range.parse::<IpAddr>().is_err() {
            return Err(Error::InvalidGlobal {
                field: "caddy_client_ip_ranges",
                reason: format!("`{range}` is not an IP address or CIDR"),
            });
        }
    }
    Ok(())
}

pub(crate) fn resolve_run(
    arguments: &RunArgs,
    current_directory: &Path,
    default_bind_address: IpAddr,
) -> Result<ResolvedRunConfig, Error> {
    let (project, ignored_local_config) = load_project(arguments, current_directory)?;
    let git_root = find_git_root(current_directory);
    let mut resolved = merge_run(
        arguments,
        project,
        git_root.as_deref(),
        current_directory,
        default_bind_address,
    )?;
    resolved.ignored_local_config = ignored_local_config;
    Ok(resolved)
}

fn load_project(
    arguments: &RunArgs,
    current_directory: &Path,
) -> Result<(Option<ProjectConfig>, Option<PathBuf>), Error> {
    if let Some(path) = arguments.config.as_deref() {
        let base = load_project_file(path, true)?;
        let local_path = path
            .parent()
            .unwrap_or_else(|| Path::new(""))
            .join("nook.local.toml");
        if local_path == path {
            return Ok((base, None));
        }
        if arguments.local {
            let local = load_project_file(&local_path, true)?;
            return Ok((merge_project(base, local), None));
        }
        let ignored = local_path.exists().then_some(local_path);
        return Ok((base, ignored));
    }

    let base = load_project_file(&current_directory.join("nook.toml"), false)?;
    let local = load_project_file(&current_directory.join("nook.local.toml"), false)?;
    Ok((merge_project(base, local), None))
}

fn load_project_file(path: &Path, required: bool) -> Result<Option<ProjectConfig>, Error> {
    let contents = if required {
        Some(read_required(path)?)
    } else {
        read_optional(path)?
    };
    let Some(contents) = contents else {
        return Ok(None);
    };
    let config: ProjectConfig = parse(path, &contents)?;
    validate_version(path, config.format_version)?;
    Ok(Some(config))
}

fn merge_project(
    base: Option<ProjectConfig>,
    local: Option<ProjectConfig>,
) -> Option<ProjectConfig> {
    let Some(local) = local else {
        return base;
    };
    let Some(base) = base else {
        return Some(local);
    };
    Some(ProjectConfig {
        format_version: local.format_version,
        name: local.name.or(base.name),
        command: local.command.or(base.command),
        tls: local.tls.or(base.tls),
        app_port: local.app_port.or(base.app_port),
        strict_port: local.strict_port.or(base.strict_port),
        readiness_warn_after_seconds: local
            .readiness_warn_after_seconds
            .or(base.readiness_warn_after_seconds),
        run_bind_address: local.run_bind_address.or(base.run_bind_address),
    })
}

fn merge_run(
    arguments: &RunArgs,
    project: Option<ProjectConfig>,
    git_root: Option<&Path>,
    current_directory: &Path,
    default_bind_address: IpAddr,
) -> Result<ResolvedRunConfig, Error> {
    let project = project.unwrap_or(ProjectConfig {
        format_version: FORMAT_VERSION,
        name: None,
        command: None,
        tls: None,
        app_port: None,
        strict_port: None,
        readiness_warn_after_seconds: None,
        run_bind_address: None,
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
        force: arguments.force,
        readiness_warn_after_seconds: arguments
            .readiness_warn_after
            .or(project.readiness_warn_after_seconds)
            .unwrap_or(DEFAULT_READINESS_WARN_AFTER_SECONDS),
        bind_address: project.run_bind_address.unwrap_or(default_bind_address),
        ignored_local_config: None,
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

pub(crate) fn infer_project_name(current_directory: &Path) -> Result<String, Error> {
    let hostname = resolve_hostname(
        None,
        None,
        find_git_root(current_directory).as_deref(),
        current_directory,
    )?;
    Ok(hostname
        .strip_suffix(".localhost")
        .unwrap_or(&hostname)
        .to_owned())
}

pub(crate) fn project_name(name: &str) -> Result<String, Error> {
    let hostname = normalize_hostname(name)?;
    Ok(hostname
        .strip_suffix(".localhost")
        .unwrap_or(&hostname)
        .to_owned())
}

pub(crate) fn normalize_hostname(name: &str) -> Result<String, Error> {
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
    let path = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    (!path.is_empty()).then(|| PathBuf::from(path))
}

#[cfg(unix)]
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

#[cfg(windows)]
fn global_config_path_with(get: impl Fn(&str) -> Option<OsString>) -> Result<PathBuf, Error> {
    get("APPDATA")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .map(|directory| directory.join("Nook/config.toml"))
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
        Error, GlobalConfig, ProjectConfig, default_run_bind_address, format_global,
        global_config_path_with, merge_run, normalize_hostname, resolve_hostname, validate_global,
        write_global_at,
    };
    use crate::cli::{Cli, Command};
    use clap::Parser;
    use std::ffi::OsString;
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    #[cfg(unix)]
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
    #[cfg(windows)]
    fn global_path_uses_roaming_app_data_on_windows() {
        let path = global_config_path_with(|key| {
            (key == "APPDATA").then(|| OsString::from(r"C:\Users\dev\AppData\Roaming"))
        })
        .unwrap();
        assert_eq!(
            path,
            Path::new(r"C:\Users\dev\AppData\Roaming\Nook\config.toml")
        );
    }

    #[test]
    fn global_defaults_include_admin_api_and_empty_overrides() {
        let config = GlobalConfig::default();
        assert_eq!(config.caddy_admin, "http://127.0.0.1:2019");
        assert!(config.https_server.is_none() && config.http_server.is_none());
        assert_eq!(config.run_bind_address.to_string(), "127.0.0.1");
        assert_eq!(config.caddy_loopback_host, "127.0.0.1");
        assert_eq!(config.caddy_client_ip_ranges, ["127.0.0.0/8", "::1"]);
    }

    #[test]
    fn global_configuration_is_serialized_and_written_safely() {
        let directory = temporary_directory();
        let path = directory.join("nested/config.toml");
        let config = GlobalConfig::default();

        write_global_at(&path, &config, false).unwrap();
        let contents = fs::read_to_string(&path).unwrap();
        assert_eq!(contents, format_global(&config).unwrap());
        assert!(contents.contains("caddy_admin = \"http://127.0.0.1:2019\""));
        assert!(write_global_at(&path, &config, false).is_err());

        let mut replacement = GlobalConfig::default();
        replacement.set_caddy_admin("unix//run/caddy/admin.socket".into());
        write_global_at(&path, &replacement, true).unwrap();
        assert!(
            fs::read_to_string(&path)
                .unwrap()
                .contains("unix//run/caddy/admin.socket")
        );
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn docker_network_settings_are_validated() {
        let mut config = GlobalConfig {
            run_bind_address: "172.30.0.1".parse().unwrap(),
            caddy_loopback_host: "host.docker.internal".into(),
            caddy_client_ip_ranges: vec!["172.30.0.1/32".into()],
            ..GlobalConfig::default()
        };
        validate_global(&config).unwrap();
        config.caddy_client_ip_ranges = vec!["not-a-range".into()];
        assert!(matches!(
            validate_global(&config),
            Err(Error::InvalidGlobal { .. })
        ));
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
            "--force",
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
            default_run_bind_address(),
        )
        .unwrap();
        assert_eq!(resolved.hostname, "cli.localhost");
        assert_eq!(resolved.command, ["cli-command"]);
        assert!(!resolved.tls);
        assert_eq!(resolved.app_port, Some(9000));
        assert!(resolved.strict_port);
        assert!(resolved.force);
        assert_eq!(resolved.readiness_warn_after_seconds, 5);
    }

    #[test]
    fn project_values_and_defaults_are_used_without_cli_values() {
        let resolved = merge_run(
            &run_args(&["run"]),
            Some(project()),
            Some(Path::new("/git")),
            Path::new("/cwd"),
            default_run_bind_address(),
        )
        .unwrap();
        assert_eq!(resolved.hostname, "project.localhost");
        assert_eq!(resolved.command, ["project-command"]);
        assert!(resolved.tls);
        assert_eq!(resolved.app_port, Some(8000));
        assert!(!resolved.strict_port);
        assert!(!resolved.force);
        assert_eq!(resolved.readiness_warn_after_seconds, 20);
    }

    #[test]
    fn command_is_required_after_merge() {
        assert!(matches!(
            merge_run(
                &run_args(&["run"]),
                None,
                Some(Path::new("/git")),
                Path::new("/cwd"),
                default_run_bind_address(),
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
            resolve_run(&run, &directory),
            Err(Error::Read { .. })
        ));
        let invalid = directory.join("invalid.toml");
        fs::write(&invalid, "format_version = 2\ncommand = [\"server\"]\n").unwrap();
        let run = run_args(&["run", "--config", invalid.to_str().unwrap()]);
        assert!(matches!(
            resolve_run(&run, &directory),
            Err(Error::UnsupportedVersion { version: 2, .. })
        ));
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn conventional_local_configuration_overrides_base_fields() {
        let directory = temporary_directory();
        fs::write(
            directory.join("nook.toml"),
            concat!(
                "format_version = 1\n",
                "name = \"base\"\n",
                "command = [\"base-command\"]\n",
                "tls = true\n",
                "app_port = 8000\n",
                "strict_port = true\n",
                "readiness_warn_after_seconds = 20\n",
                "run_bind_address = \"127.0.0.2\"\n",
            ),
        )
        .unwrap();
        fs::write(
            directory.join("nook.local.toml"),
            concat!(
                "format_version = 1\n",
                "name = \"local\"\n",
                "command = [\"local-command\"]\n",
                "tls = false\n",
                "strict_port = false\n",
                "readiness_warn_after_seconds = 5\n",
                "run_bind_address = \"0.0.0.0\"\n",
            ),
        )
        .unwrap();

        let resolved = resolve_run(&run_args(&["run"]), &directory).unwrap();
        assert_eq!(resolved.hostname, "local.localhost");
        assert_eq!(resolved.command, ["local-command"]);
        assert!(!resolved.tls);
        assert_eq!(resolved.app_port, Some(8000));
        assert!(!resolved.strict_port);
        assert_eq!(resolved.readiness_warn_after_seconds, 5);
        assert_eq!(resolved.bind_address.to_string(), "0.0.0.0");
        assert!(resolved.ignored_local_config.is_none());

        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn conventional_local_configuration_can_exist_without_base() {
        let directory = temporary_directory();
        fs::write(
            directory.join("nook.local.toml"),
            "format_version = 1\nname = \"local-only\"\ncommand = [\"server\"]\n",
        )
        .unwrap();

        let resolved = resolve_run(&run_args(&["run"]), &directory).unwrap();
        assert_eq!(resolved.hostname, "local-only.localhost");
        assert_eq!(resolved.command, ["server"]);

        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn explicit_configuration_ignores_or_applies_its_local_neighbor() {
        let directory = temporary_directory();
        let base = directory.join("custom.toml");
        let local = directory.join("nook.local.toml");
        fs::write(
            &base,
            "format_version = 1\nname = \"base\"\ncommand = [\"server\"]\n",
        )
        .unwrap();
        fs::write(&local, "format_version = 1\nname = \"local\"\n").unwrap();

        let base_path = base.to_str().unwrap();
        let ignored = resolve_run(&run_args(&["run", "--config", base_path]), &directory).unwrap();
        assert_eq!(ignored.hostname, "base.localhost");
        assert_eq!(
            ignored.ignored_local_config.as_deref(),
            Some(local.as_path())
        );

        let applied = resolve_run(
            &run_args(&["run", "--config", base_path, "--local"]),
            &directory,
        )
        .unwrap();
        assert_eq!(applied.hostname, "local.localhost");
        assert!(applied.ignored_local_config.is_none());

        fs::remove_file(&local).unwrap();
        assert!(matches!(
            resolve_run(
                &run_args(&["run", "--config", base_path, "--local"]),
                &directory
            ),
            Err(Error::Read { path, .. }) if path == local
        ));

        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn explicit_local_file_is_not_loaded_twice() {
        let directory = temporary_directory();
        let local = directory.join("nook.local.toml");
        fs::write(
            &local,
            "format_version = 1\nname = \"local\"\ncommand = [\"server\"]\n",
        )
        .unwrap();
        let resolved = resolve_run(
            &run_args(&["run", "--config", local.to_str().unwrap(), "--local"]),
            &directory,
        )
        .unwrap();
        assert_eq!(resolved.hostname, "local.localhost");
        assert!(resolved.ignored_local_config.is_none());
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn invalid_local_configuration_reports_its_own_path() {
        let directory = temporary_directory();
        let local = directory.join("nook.local.toml");
        fs::write(&local, "format_version = 2\ncommand = [\"server\"]\n").unwrap();
        assert!(matches!(
            resolve_run(&run_args(&["run"]), &directory),
            Err(Error::UnsupportedVersion { path, version: 2 }) if path == local
        ));
        fs::write(&local, "format_version = 1\nunknown = true\n").unwrap();
        assert!(matches!(
            resolve_run(&run_args(&["run", "--", "server"]), &directory),
            Err(Error::Parse { path, .. }) if path == local
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
            run_bind_address: None,
        }
    }

    fn resolve_run(
        arguments: &crate::cli::RunArgs,
        current_directory: &Path,
    ) -> Result<super::ResolvedRunConfig, Error> {
        super::resolve_run(arguments, current_directory, default_run_bind_address())
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
