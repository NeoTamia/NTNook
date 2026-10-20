//! Native workspace manifests and member configuration resolution.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::net::IpAddr;
use std::path::{Component, Path, PathBuf};

use serde::Deserialize;

use crate::{cli, config};

#[derive(Debug)]
pub(crate) struct ResolvedWorkspaceConfig {
    pub(crate) root: PathBuf,
    pub(crate) apps: Vec<WorkspaceApp>,
}

#[derive(Debug)]
pub(crate) struct WorkspaceApp {
    pub(crate) id: String,
    pub(crate) directory: PathBuf,
    pub(crate) config: config::ResolvedRunConfig,
    pub(crate) environment: BTreeMap<String, String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    format_version: u32,
    name: Option<String>,
    command: Option<Vec<String>>,
    #[serde(default)]
    apps: Vec<Member>,
}

#[derive(Deserialize)]
struct Member {
    id: String,
    dir: Option<PathBuf>,
    #[serde(flatten)]
    settings: toml::Table,
}

/// Returns `None` for absent or single-application configurations.
pub(crate) fn load(
    arguments: &cli::RunArgs,
    current_directory: &Path,
    default_bind_address: IpAddr,
) -> crate::Result<Option<ResolvedWorkspaceConfig>> {
    load_inner(arguments, current_directory, default_bind_address).map_err(Into::into)
}

fn load_inner(
    arguments: &cli::RunArgs,
    current_directory: &Path,
    default_bind_address: IpAddr,
) -> Result<Option<ResolvedWorkspaceConfig>, config::Error> {
    let mut path = arguments.config.as_ref().map_or_else(
        || current_directory.join("nook.toml"),
        |path| current_directory.join(path),
    );
    let contents = if arguments.config.is_some() {
        Some(config::read_required(&path)?)
    } else {
        match config::read_optional(&path)? {
            Some(contents) => Some(contents),
            None => {
                path = current_directory.join("nook.local.toml");
                config::read_optional(&path)?
            }
        }
    };
    let Some(contents) = contents else {
        return Ok(None);
    };
    if !is_workspace(&path, &contents)? {
        return Ok(None);
    }
    let path = fs::canonicalize(&path).map_err(|source| config::Error::Read {
        path: path.clone(),
        source,
    })?;
    let mut manifest: Manifest = config::parse(&path, &contents)?;
    debug_assert_eq!(manifest.format_version, 2);
    if manifest.command.is_some() {
        return Err(invalid(
            &path,
            "format_version = 2 uses [[apps]]; remove the top-level command",
        ));
    }
    if manifest.apps.is_empty() {
        return Err(invalid(
            &path,
            "format_version = 2 requires at least one [[apps]] entry",
        ));
    }
    let root = canonical_directory(path.parent().unwrap_or(current_directory))?;
    let local_path = root.join("nook.local.toml");
    let use_local = arguments.config.is_none() || arguments.local;
    let ignored_root_local =
        (!use_local && local_path.exists() && local_path != path).then(|| local_path.clone());
    if use_local && local_path != path {
        let local = if arguments.local {
            Some(config::read_required(&local_path)?)
        } else {
            config::read_optional(&local_path)?
        };
        if let Some(contents) = local
            && is_workspace(&local_path, &contents)?
        {
            let local: Manifest = config::parse(&local_path, &contents)?;
            if local.command.is_some() {
                return Err(invalid(
                    &local_path,
                    "remove the top-level command; workspace overrides use [[apps]]",
                ));
            }
            manifest.name = local.name.or(manifest.name);
            let mut overridden = BTreeSet::new();
            for member in local.apps {
                if !overridden.insert(member.id.clone()) {
                    return Err(invalid(
                        &local_path,
                        format!("duplicate app id `{}`", member.id),
                    ));
                }
                let base = manifest
                    .apps
                    .iter_mut()
                    .find(|base| base.id == member.id)
                    .ok_or_else(|| {
                        invalid(
                            &local_path,
                            format!("local override references undeclared app `{}`", member.id),
                        )
                    })?;
                if let Some(dir) = member.dir {
                    base.dir = Some(dir);
                }
                merge_settings(&mut base.settings, member.settings);
            }
        }
    }
    // A configured workspace name is its namespace; a CLI --name overrides it.
    let workspace_name = config::project_name(
        arguments
            .name
            .as_deref()
            .or(manifest.name.as_deref())
            .or_else(|| root.file_name().and_then(|name| name.to_str()))
            .ok_or(config::Error::MissingName)?,
    )?;
    let mut ids = BTreeSet::new();
    let mut apps: Vec<WorkspaceApp> = Vec::new();
    for member in manifest.apps {
        validate_id(&path, &member.id)?;
        if !ids.insert(member.id.to_ascii_lowercase()) {
            return Err(invalid(
                &path,
                format!(
                    "duplicate app id `{}` (IDs are case-insensitive)",
                    member.id
                ),
            ));
        }
        let dir = member.dir.ok_or_else(|| {
            invalid(
                &path,
                format!("app `{}` requires a relative dir", member.id),
            )
        })?;
        if dir.as_os_str().is_empty()
            || dir.is_absolute()
            || dir
                .components()
                .any(|component| matches!(component, Component::RootDir | Component::Prefix(_)))
        {
            return Err(invalid(
                &path,
                format!(
                    "app `{}` dir must be relative and inside the workspace root",
                    member.id
                ),
            ));
        }
        let directory = canonical_directory(&root.join(&dir))?;
        if !directory.starts_with(&root) {
            return Err(invalid(
                &path,
                format!(
                    "app `{}` dir resolves outside the workspace root",
                    member.id
                ),
            ));
        }
        if let Some(other) = apps.iter().find(|app| {
            directory.starts_with(&app.directory) || app.directory.starts_with(&directory)
        }) {
            return Err(invalid(
                &path,
                format!(
                    "apps `{}` and `{}` have duplicate or overlapping directories",
                    other.id, member.id
                ),
            ));
        }
        let mut settings = member.settings;
        if settings.contains_key("format_version") {
            return Err(invalid(
                &path,
                format!(
                    "app `{}` must not set format_version; use the root version",
                    member.id
                ),
            ));
        }
        settings.insert("format_version".into(), toml::Value::Integer(1));
        let project: config::ProjectConfig =
            toml::Value::Table(settings)
                .try_into()
                .map_err(|source| config::Error::Parse {
                    path: path.clone(),
                    source,
                })?;
        let member_path = directory.join("nook.toml");
        // A root member points at the manifest itself, not a second v1 config.
        let base = if member_path == path {
            None
        } else {
            config::load_project_file(&member_path, false)?
        };
        let mut project = config::merge_project(Some(project), base).unwrap();
        let member_local = directory.join("nook.local.toml");
        let local = if use_local && member_local != path {
            match config::read_optional(&member_local)? {
                Some(contents) if directory == root && is_workspace(&member_local, &contents)? => {
                    None
                }
                Some(_) => config::load_project_file(&member_local, false)?,
                None => None,
            }
        } else {
            None
        };
        project = config::merge_project(Some(project), local).unwrap();
        if project.name.is_none() {
            project.name = Some(format!("{}.{}", member.id, workspace_name));
        }
        let hostname = config::normalize_hostname(project.name.as_deref().unwrap())?;
        let environment = project.env.take().unwrap_or_default();
        let mut resolved = config::merge_run(
            arguments,
            Some(project),
            None,
            &directory,
            default_bind_address,
        )?;
        // --name names the workspace; explicit member names remain authoritative.
        resolved.hostname = hostname;
        resolved.ignored_local_config = ignored_root_local.clone().or_else(|| {
            (!use_local && member_local.exists() && member_local != path).then_some(member_local)
        });
        if resolved.command.is_empty() {
            return Err(invalid(
                &path,
                format!("app `{}` requires a nonempty command argv", member.id),
            ));
        }
        if resolved.tailscale || resolved.funnel || resolved.lan {
            return Err(invalid(
                &path,
                format!(
                    "app `{}`: Tailscale, Funnel, and LAN exposure are not supported by native workspaces yet; disable them with --no-tailscale and --no-lan",
                    member.id
                ),
            ));
        }
        if let Some(other) = apps
            .iter()
            .find(|app| app.config.hostname == resolved.hostname)
        {
            return Err(invalid(
                &path,
                format!(
                    "apps `{}` and `{}` resolve to the same hostname `{}`",
                    other.id, member.id, resolved.hostname
                ),
            ));
        }
        apps.push(WorkspaceApp {
            id: member.id,
            directory,
            config: resolved,
            environment,
        });
    }
    validate_environment(&path, &apps)?;
    Ok(Some(ResolvedWorkspaceConfig { root, apps }))
}

fn is_workspace(path: &Path, contents: &str) -> Result<bool, config::Error> {
    #[derive(Deserialize)]
    struct Version {
        format_version: u32,
    }
    let version: Version = config::parse(path, contents)?;
    match version.format_version {
        1 => {
            let _: config::ProjectConfig = config::parse(path, contents)?;
            Ok(false)
        }
        2 => Ok(true),
        version => Err(invalid(
            path,
            format!("unsupported format_version {version}; expected 1 or 2"),
        )),
    }
}

fn canonical_directory(path: &Path) -> Result<PathBuf, config::Error> {
    let directory = fs::canonicalize(path).map_err(|source| config::Error::Read {
        path: path.to_owned(),
        source,
    })?;
    if !directory.is_dir() {
        return Err(invalid(path, "app dir must be an existing directory"));
    }
    Ok(directory)
}

fn validate_id(path: &Path, id: &str) -> Result<(), config::Error> {
    if id.is_empty()
        || id.len() > 63
        || id.starts_with('-')
        || id.ends_with('-')
        || !id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
    {
        return Err(invalid(
            path,
            format!(
                "app id `{id}` must be a single DNS-safe label (1–63 ASCII letters, digits, or hyphens; no leading or trailing hyphen)"
            ),
        ));
    }
    Ok(())
}

fn merge_settings(base: &mut toml::Table, mut local: toml::Table) {
    if let Some(env) = local.remove("env") {
        if let (Some(toml::Value::Table(base_env)), toml::Value::Table(local_env)) =
            (base.get_mut("env"), &env)
        {
            base_env.extend(local_env.clone());
        } else {
            base.insert("env".into(), env);
        }
    }
    base.extend(local);
}

fn validate_environment(path: &Path, apps: &[WorkspaceApp]) -> Result<(), config::Error> {
    let mut graph = BTreeMap::<&str, BTreeSet<&str>>::new();
    for app in apps {
        let references = graph.entry(&app.id).or_default();
        for (name, value) in &app.environment {
            if name.is_empty() || name.contains(['=', '\0']) || value.contains('\0') {
                return Err(invalid(
                    path,
                    format!("app `{}` has an invalid environment entry `{name}`", app.id),
                ));
            }
            let mut remaining = value.as_str();
            while let Some((_, token)) = remaining.split_once("${") {
                let (token, rest) = token.split_once('}').ok_or_else(|| {
                    invalid(
                        path,
                        format!(
                            "app `{}` env `{name}` contains an unterminated reference",
                            app.id
                        ),
                    )
                })?;
                let (id, field) = token.rsplit_once('.').ok_or_else(|| invalid(path, format!("invalid environment token `${{{token}}}`; expected ${{id.url}}, ${{id.upstream}}, ${{id.host}}, or ${{id.port}}")))?;
                if !matches!(field, "url" | "upstream" | "host" | "port") {
                    return Err(invalid(
                        path,
                        format!("unsupported environment token `${{{token}}}`"),
                    ));
                }
                let target = apps.iter().find(|app| app.id == id).ok_or_else(|| {
                    invalid(
                        path,
                        format!(
                            "app `{}` env `{name}` references undeclared app `{id}`",
                            app.id
                        ),
                    )
                })?;
                references.insert(&target.id);
                remaining = rest;
            }
        }
    }
    fn visit<'a>(
        id: &'a str,
        graph: &BTreeMap<&'a str, BTreeSet<&'a str>>,
        active: &mut BTreeSet<&'a str>,
        done: &mut BTreeSet<&'a str>,
    ) -> bool {
        if done.contains(id) {
            return true;
        }
        if !active.insert(id) {
            return false;
        }
        if graph[id]
            .iter()
            .any(|target| !visit(target, graph, active, done))
        {
            return false;
        }
        active.remove(id);
        done.insert(id);
        true
    }
    let mut done = BTreeSet::new();
    for app in apps {
        if !visit(&app.id, &graph, &mut BTreeSet::new(), &mut done) {
            return Err(invalid(
                path,
                format!("environment reference cycle involving app `{}`", app.id),
            ));
        }
    }
    Ok(())
}

fn invalid(path: &Path, reason: impl Into<String>) -> config::Error {
    config::Error::InvalidWorkspace {
        path: path.to_owned(),
        reason: reason.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::load;
    use crate::cli::{Cli, Command, RunArgs};
    use clap::Parser;
    use std::fs;
    use std::path::PathBuf;

    struct Directory(PathBuf);

    impl Directory {
        fn new() -> Self {
            let directory = std::env::temp_dir()
                .join(format!("nook-workspace-config-{}", uuid::Uuid::new_v4()));
            fs::create_dir_all(directory.join("apps/api/nested")).unwrap();
            fs::create_dir_all(directory.join("apps/web")).unwrap();
            Self(fs::canonicalize(directory).unwrap())
        }

        fn write(&self, path: &str, contents: &str) {
            fs::write(self.0.join(path), contents).unwrap();
        }

        fn workspace(&self, apps: &str) {
            self.write(
                "nook.toml",
                &format!("format_version = 2\nname = \"demo\"\n{apps}"),
            );
        }

        fn load(&self, args: &[&str]) -> crate::Result<Option<super::ResolvedWorkspaceConfig>> {
            load(&arguments(args), &self.0, "127.0.0.2".parse().unwrap())
        }

        fn error(&self, expected: &str) {
            let message = self.load(&[]).unwrap_err().to_string();
            assert!(
                message.contains(expected),
                "expected {expected:?}, got {message:?}"
            );
        }
    }

    impl Drop for Directory {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.0).unwrap();
        }
    }

    const API: &str = "[[apps]]\nid = \"api\"\ndir = \"apps/api\"\ncommand = [\"api-server\"]\n";
    const WEB: &str = "[[apps]]\nid = \"web\"\ndir = \"apps/web\"\ncommand = [\"web-server\"]\n";

    #[test]
    fn absent_and_v1_configs_remain_standalone_and_v1_rejects_apps() {
        let directory = Directory::new();
        assert!(directory.load(&[]).unwrap().is_none());
        directory.write("nook.toml", "format_version = 1\ncommand = [\"server\"]\n");
        assert!(directory.load(&[]).unwrap().is_none());
        directory.write("nook.toml", &format!("format_version = 1\n{API}"));
        directory.error("unknown field `apps`");
        directory.write("nook.toml", "format_version = 3\n");
        directory.error("expected 1 or 2");
    }

    #[test]
    fn conventional_local_only_workspace_is_supported() {
        let directory = Directory::new();
        directory.write("nook.local.toml", &format!("format_version = 2\n{API}"));
        let workspace = directory.load(&[]).unwrap().unwrap();
        assert_eq!(workspace.apps.len(), 1);
        assert_eq!(workspace.apps[0].id, "api");
        directory.write(
            "nook.local.toml",
            "format_version = 1\ncommand = [\"server\"]\n",
        );
        assert!(directory.load(&[]).unwrap().is_none());
    }

    #[test]
    fn workspace_schema_is_strict_and_requires_apps_and_member_commands() {
        let directory = Directory::new();
        directory.workspace("");
        directory.error("at least one [[apps]]");
        directory.workspace(&format!("command = [\"root-server\"]\n{API}"));
        directory.error("top-level command");
        directory.workspace(&format!("unknown = true\n{API}"));
        directory.error("unknown field");
        directory.workspace(&format!("{API}unknown = true\n"));
        directory.error("unknown field");
        directory.workspace("[[apps]]\nid = \"api\"\ndir = \"apps/api\"\n");
        directory.error("a child command is required");
        directory.workspace("[[apps]]\nid = \"api\"\ndir = \"apps/api\"\ncommand = []\n");
        directory.error("nonempty command");
        directory.workspace("[[apps]]\nid = \"api\"\ndir = \"apps/api\"\ncommand = \"server\"\n");
        directory.error("invalid type");
        directory.workspace(&format!("{API}format_version = 1\n"));
        directory.error("must not set format_version");
    }

    #[test]
    fn merges_member_project_local_environment_and_cli_in_order() {
        let directory = Directory::new();
        directory.workspace(&format!("{API}app_port = 7000\ntls = true\n[apps.env]\nKEEP = \"root\"\nOVERRIDE = \"root\"\n{WEB}"));
        directory.write("apps/api/nook.toml", "format_version = 1\nname = \"explicit-api\"\ncommand = [\"project-server\"]\napp_port = 7100\nframework = \"vite\"\n[env]\nPROJECT = \"base\"\nOVERRIDE = \"project\"\n");
        directory.write("apps/api/nook.local.toml", "format_version = 1\ncommand = [\"local-server\"]\napp_port = 7200\ntls = false\n[env]\nLOCAL = \"local\"\nOVERRIDE = \"local\"\n");
        let workspace = directory.load(&[]).unwrap().unwrap();
        assert_eq!(workspace.root, directory.0);
        let api = &workspace.apps[0];
        assert_eq!(api.directory, directory.0.join("apps/api"));
        assert_eq!(api.config.hostname, "explicit-api.localhost");
        assert_eq!(api.config.command, ["local-server"]);
        assert_eq!(api.config.app_port, Some(7200));
        assert!(!api.config.tls);
        assert_eq!(api.config.bind_address.to_string(), "127.0.0.2");
        assert_eq!(api.environment.len(), 4);
        assert_eq!(api.environment["KEEP"], "root");
        assert_eq!(api.environment["OVERRIDE"], "local");
        assert_eq!(workspace.apps[1].config.hostname, "web.demo.localhost");
        let workspace = directory
            .load(&[
                "--name",
                "cli",
                "--app-port",
                "9000",
                "--run-bind-address",
                "::1",
                "--",
                "cli-server",
            ])
            .unwrap()
            .unwrap();
        assert_eq!(workspace.apps[0].config.hostname, "explicit-api.localhost");
        assert_eq!(workspace.apps[1].config.hostname, "web.cli.localhost");
        for app in workspace.apps {
            assert_eq!(app.config.app_port, Some(9000));
            assert_eq!(app.config.bind_address.to_string(), "::1");
            assert_eq!(app.config.command, ["cli-server"]);
        }
    }

    #[test]
    fn explicit_config_ignores_local_files_until_local_is_requested() {
        let directory = Directory::new();
        directory.write("custom.toml", &format!("format_version = 2\nname = \"demo\"\n{API}[apps.env]\nKEEP = \"root\"\nOVERRIDE = \"root\"\n"));
        directory.write("nook.local.toml", "format_version = 2\n[[apps]]\nid = \"api\"\n[apps.env]\nOVERRIDE = \"root-local\"\nADDED = \"root-local\"\n");
        directory.write(
            "apps/api/nook.local.toml",
            "format_version = 1\nname = \"member-local\"\n[env]\nOVERRIDE = \"member-local\"\n",
        );
        let ignored = directory
            .load(&["--config", "custom.toml"])
            .unwrap()
            .unwrap();
        assert_eq!(ignored.apps[0].environment["OVERRIDE"], "root");
        assert_eq!(
            ignored.apps[0].config.ignored_local_config.as_deref(),
            Some(directory.0.join("nook.local.toml").as_path())
        );
        let applied = directory
            .load(&["--config", "custom.toml", "--local"])
            .unwrap()
            .unwrap();
        assert_eq!(applied.apps[0].config.hostname, "member-local.localhost");
        assert_eq!(applied.apps[0].environment["OVERRIDE"], "member-local");
        assert_eq!(applied.apps[0].environment["KEEP"], "root");
        assert_eq!(applied.apps[0].environment["ADDED"], "root-local");
        assert!(applied.apps[0].config.ignored_local_config.is_none());
        fs::remove_file(directory.0.join("nook.local.toml")).unwrap();
        assert!(
            directory
                .load(&["--config", "custom.toml", "--local"])
                .unwrap_err()
                .to_string()
                .contains("nook.local.toml")
        );
        assert!(directory.load(&["--config", "missing.toml"]).is_err());
    }

    #[test]
    fn workspace_local_env_overrides_by_member_without_replacing_other_members() {
        let directory = Directory::new();
        directory.workspace(&format!(
            "{API}{WEB}[apps.env]\nKEEP = \"root\"\nOVERRIDE = \"root\"\n"
        ));
        directory.write(
            "nook.local.toml",
            "format_version = 2\n[[apps]]\nid = \"web\"\n[apps.env]\nOVERRIDE = \"local\"\n",
        );
        let workspace = directory.load(&[]).unwrap().unwrap();
        assert_eq!(workspace.apps.len(), 2);
        assert_eq!(workspace.apps[1].environment["KEEP"], "root");
        assert_eq!(workspace.apps[1].environment["OVERRIDE"], "local");
        directory.write(
            "nook.local.toml",
            "format_version = 2\n[[apps]]\nid = \"unknown\"\n",
        );
        directory.error("undeclared app");
    }

    #[test]
    fn root_member_and_explicit_local_file_are_not_loaded_as_v1() {
        let directory = Directory::new();
        directory.workspace("[[apps]]\nid = \"root\"\ndir = \".\"\ncommand = [\"server\"]\n");
        assert_eq!(
            directory.load(&[]).unwrap().unwrap().apps[0].directory,
            directory.0
        );
        directory.write("nook.local.toml", &format!("format_version = 2\n{API}"));
        assert!(
            directory
                .load(&["--config", "nook.local.toml", "--local"])
                .is_ok()
        );
    }

    #[test]
    fn ids_directories_and_hostnames_are_unambiguous() {
        let directory = Directory::new();
        for id in [
            "",
            "api.web",
            "bad_id",
            "-api",
            "api-",
            "équipe",
            &"a".repeat(64),
        ] {
            directory.workspace(&API.replace("id = \"api\"", &format!("id = {id:?}")));
            directory.error("single DNS-safe label");
        }
        directory.workspace(&format!(
            "{API}{}",
            WEB.replace("id = \"web\"", "id = \"API\"")
        ));
        directory.error("duplicate app id");
        for dir in ["apps/api", "apps/api/nested", "."] {
            directory.workspace(&format!("{API}{}", WEB.replace("apps/web", dir)));
            directory.error("overlapping directories");
        }
        for dir in ["", "../outside", directory.0.to_str().unwrap()] {
            directory.workspace(&API.replace("apps/api", dir));
            assert!(directory.load(&[]).is_err(), "{dir} should fail");
        }
        directory.workspace(&format!("{API}name = \"same\"\n{WEB}name = \"same\"\n"));
        directory.error("same hostname");
        directory.write("apps/api/nook.toml", "format_version = 2\n");
        directory.workspace(API);
        directory.error("expected 1");
    }

    #[test]
    #[cfg(unix)]
    fn canonical_paths_reject_symlink_escapes_and_aliases() {
        use std::os::unix::fs::symlink;
        let directory = Directory::new();
        let outside = Directory::new();
        symlink(&outside.0, directory.0.join("escape")).unwrap();
        directory.workspace(&API.replace("apps/api", "escape"));
        directory.error("outside the workspace root");
        symlink(directory.0.join("apps/api"), directory.0.join("alias")).unwrap();
        directory.workspace(&format!("{API}{}", WEB.replace("apps/web", "alias")));
        directory.error("overlapping directories");
    }

    #[test]
    fn exposure_options_fail_before_a_workspace_is_returned() {
        let directory = Directory::new();
        directory.workspace(API);
        for flag in ["--tailscale", "--funnel", "--lan"] {
            assert!(
                directory
                    .load(&[flag])
                    .unwrap_err()
                    .to_string()
                    .contains("not supported by native workspaces")
            );
        }
        for field in ["tailscale", "funnel", "lan"] {
            directory.workspace(&format!("{API}{field} = true\n"));
            directory.error("not supported by native workspaces");
            assert!(directory.load(&["--no-tailscale", "--no-lan"]).is_ok());
        }
    }

    #[test]
    fn environment_tokens_are_validated_without_expanding_allocated_ports() {
        let directory = Directory::new();
        directory.workspace(&format!("{API}{WEB}[apps.env]\nENDPOINT = \"${{api.url}} ${{api.upstream}} ${{api.host}}:${{api.port}}\"\n"));
        let workspace = directory.load(&[]).unwrap().unwrap();
        assert!(workspace.apps[1].environment["ENDPOINT"].contains("${api.port}"));
        for (token, error) in [
            ("${unknown.url}", "undeclared app"),
            ("${api.invalid}", "unsupported environment token"),
            ("${api}", "invalid environment token"),
            ("${api.url", "unterminated reference"),
        ] {
            directory.workspace(&format!("{API}{WEB}[apps.env]\nENDPOINT = {token:?}\n"));
            directory.error(error);
        }
        directory.workspace(&format!("{API}[apps.env]\nSELF = \"${{api.url}}\"\n"));
        directory.error("reference cycle");
        directory.workspace(&format!(
            "{API}[apps.env]\nWEB = \"${{web.url}}\"\n{WEB}[apps.env]\nAPI = \"${{api.url}}\"\n"
        ));
        directory.error("reference cycle");
    }

    #[test]
    fn workspace_name_defaults_to_its_directory_and_member_v1_stays_standalone() {
        let directory = Directory::new();
        directory.write("nook.toml", &format!("format_version = 2\n{API}"));
        let workspace = directory.load(&[]).unwrap().unwrap();
        assert_eq!(
            workspace.apps[0].config.hostname,
            format!(
                "api.{}.localhost",
                directory.0.file_name().unwrap().to_str().unwrap()
            )
        );
        directory.write(
            "apps/api/nook.toml",
            "format_version = 1\nname = \"standalone\"\ncommand = [\"server\"]\n",
        );
        assert!(
            load(
                &arguments(&[]),
                &directory.0.join("apps/api"),
                "127.0.0.1".parse().unwrap()
            )
            .unwrap()
            .is_none()
        );
        let standalone = crate::config::resolve_run(
            &arguments(&[]),
            &directory.0.join("apps/api"),
            "127.0.0.1".parse().unwrap(),
        )
        .unwrap();
        assert_eq!(standalone.hostname, "standalone.localhost");
    }

    fn arguments(args: &[&str]) -> RunArgs {
        let cli =
            Cli::try_parse_from(["nook", "run"].into_iter().chain(args.iter().copied())).unwrap();
        let Command::Run(arguments) = cli.command else {
            unreachable!()
        };
        arguments
    }
}
