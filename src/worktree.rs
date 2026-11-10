//! Opt-in Git worktree identity, independent of HEAD and the invocation directory.

use std::path::{Path, PathBuf};
use std::process::Command;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::config::Error;

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(untagged)]
pub(crate) enum Setting {
    Enabled(bool),
    Named(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Identity {
    pub(crate) key: String,
    pub(crate) label: String,
}

pub(crate) fn setting(
    cli: &Option<String>,
    disabled: bool,
    file: Option<Setting>,
) -> Option<Setting> {
    if disabled {
        Some(Setting::Enabled(false))
    } else if let Some(label) = cli {
        Some(if label.is_empty() {
            Setting::Enabled(true)
        } else {
            Setting::Named(label.clone())
        })
    } else {
        file
    }
}

pub(crate) fn validate_label(label: &str) -> Result<String, Error> {
    if label.is_empty()
        || label.len() > 63
        || label.starts_with('-')
        || label.ends_with('-')
        || !label
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-')
    {
        return Err(Error::InvalidWorktree(
            "worktree identifier must be one DNS label (1–63 ASCII letters, digits or hyphens, without leading/trailing hyphens)".into(),
        ));
    }
    Ok(label.to_ascii_lowercase())
}

pub(crate) fn resolve(
    setting: Option<&Setting>,
    directory: &Path,
) -> Result<Option<Identity>, Error> {
    let Some(setting) = setting else {
        return Ok(None);
    };
    if setting == &Setting::Enabled(false) {
        return Ok(None);
    }
    let explicit = match setting {
        Setting::Named(label) => Some(validate_label(label)?),
        _ => None,
    };
    let root = git_path(directory, "--show-toplevel")?;
    let git_dir = git_path(directory, "--absolute-git-dir")?;
    let git_dir = std::fs::canonicalize(&git_dir).map_err(|source| Error::Read {
        path: git_dir,
        source,
    })?;
    let key: String = Sha256::digest(git_dir.as_os_str().as_encoded_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    let label = explicit.unwrap_or_else(|| {
        let name = root.file_name().unwrap_or_default().to_string_lossy();
        let slug: String = name
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() {
                    c.to_ascii_lowercase()
                } else {
                    '-'
                }
            })
            .take(50)
            .collect();
        let slug = slug.trim_matches('-');
        format!(
            "{}-{}",
            if slug.is_empty() { "worktree" } else { slug },
            &key[..12]
        )
    });
    Ok(Some(Identity { key, label }))
}

fn git_path(directory: &Path, argument: &str) -> Result<PathBuf, Error> {
    let output = Command::new("git")
        .arg("-C")
        .arg(directory)
        .args(["rev-parse", argument])
        .output()
        .map_err(|error| Error::InvalidWorktree(format!("cannot run git: {error}")))?;
    if !output.status.success() {
        return Err(Error::InvalidWorktree(format!(
            "worktree mode requires a Git working tree: {}",
            directory.display()
        )));
    }
    let mut bytes = output.stdout;
    if bytes.last() == Some(&b'\n') {
        bytes.pop();
    }
    if bytes.last() == Some(&b'\r') {
        bytes.pop();
    }
    #[cfg(unix)]
    let path = {
        use std::os::unix::ffi::OsStringExt;
        PathBuf::from(std::ffi::OsString::from_vec(bytes))
    };
    #[cfg(windows)]
    let path = PathBuf::from(
        String::from_utf8(bytes)
            .map_err(|_| Error::InvalidWorktree("Git returned a non-UTF-8 path".into()))?,
    );
    Ok(path)
}

/// Keep the workspace namespace and explicit app names intact.
pub(crate) fn hostname(
    hostname: &str,
    identity: &Identity,
    namespace: Option<&str>,
) -> Result<String, Error> {
    let name = hostname.strip_suffix(".localhost").unwrap_or(hostname);
    let scoped = match namespace.and_then(|ns| name.strip_suffix(&format!(".{ns}"))) {
        Some(app) => format!("{app}.{}.{}", identity.label, namespace.unwrap()),
        None => format!("{}.{name}", identity.label),
    };
    crate::config::normalize_hostname(&scoped)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_preserve_explicit_false_and_validate_dns_labels() {
        assert_eq!(
            setting(&None, false, Some(Setting::Enabled(false))),
            Some(Setting::Enabled(false))
        );
        assert_eq!(
            setting(
                &Some(String::new()),
                false,
                Some(Setting::Named("local".into()))
            ),
            Some(Setting::Enabled(true))
        );
        assert_eq!(
            setting(&None, true, Some(Setting::Enabled(true))),
            Some(Setting::Enabled(false))
        );
        for invalid in ["", "a.b", "a_b", "-a", "a-", "é"] {
            assert!(validate_label(invalid).is_err(), "{invalid}");
        }
        assert!(validate_label(&"a".repeat(64)).is_err());
        assert_eq!(validate_label("Agent-A").unwrap(), "agent-a");
    }

    #[test]
    fn config_precedence_and_workspace_namespace_are_resolved_before_spawn() {
        use clap::Parser;
        let temp =
            std::env::temp_dir().join(format!("nook-worktree-config-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(temp.join("api")).unwrap();
        assert!(
            Command::new("git")
                .arg("init")
                .arg(&temp)
                .output()
                .unwrap()
                .status
                .success()
        );
        let args = |flags: &[&str]| {
            let cli = crate::cli::Cli::try_parse_from(
                ["nook", "run"].into_iter().chain(flags.iter().copied()),
            )
            .unwrap();
            let crate::cli::Command::Run(args) = cli.command else {
                unreachable!()
            };
            args
        };
        let resolve = |flags: &[&str]| {
            crate::config::resolve_run(&args(flags), &temp, "127.0.0.1".parse().unwrap())
        };
        std::fs::write(
            temp.join("nook.toml"),
            "format_version = 1\nname = \"demo\"\ncommand = [\"server\"]\nworktree = true\n",
        )
        .unwrap();
        let auto = resolve(&[]).unwrap();
        assert!(auto.worktree.is_some());
        std::fs::write(
            temp.join("nook.local.toml"),
            "format_version = 1\nworktree = \"agent-a\"\n",
        )
        .unwrap();
        assert_eq!(resolve(&[]).unwrap().hostname, "agent-a.demo.localhost");
        assert_eq!(
            resolve(&["--worktree=agent-b"]).unwrap().hostname,
            "agent-b.demo.localhost"
        );
        assert_eq!(
            resolve(&["--no-worktree"]).unwrap().hostname,
            "demo.localhost"
        );
        assert_eq!(resolve(&["--worktree"]).unwrap().hostname, auto.hostname);
        assert!(
            resolve(&["--funnel"])
                .unwrap_err()
                .to_string()
                .contains("Funnel")
        );
        assert!(resolve(&["--no-worktree", "--funnel"]).unwrap().funnel);
        std::fs::write(
            temp.join("nook.local.toml"),
            "format_version = 1\nworktree = false\n",
        )
        .unwrap();
        assert!(resolve(&[]).unwrap().worktree.is_none());
        std::fs::write(temp.join("nook.toml"), "format_version = 2\nname = \"demo\"\nworktree = true\n[[apps]]\nid = \"api\"\ndir = \"api\"\ncommand = [\"server\"]\n").unwrap();
        std::fs::write(
            temp.join("nook.local.toml"),
            "format_version = 2\nworktree = \"agent-a\"\n",
        )
        .unwrap();
        let workspace = |flags: &[&str]| {
            crate::workspace_config::load(&args(flags), &temp, "127.0.0.1".parse().unwrap())
        };
        assert_eq!(
            workspace(&[]).unwrap().unwrap().apps[0].config.hostname,
            "api.agent-a.demo.localhost"
        );
        assert_eq!(
            workspace(&["--no-worktree"]).unwrap().unwrap().apps[0]
                .config
                .hostname,
            "api.demo.localhost"
        );
        assert!(
            workspace(&["--funnel"])
                .unwrap_err()
                .to_string()
                .contains("Funnel")
        );
        let config_path = temp.join("nook.toml");
        let explicit = workspace(&["--config", config_path.to_str().unwrap()])
            .unwrap()
            .unwrap();
        assert_ne!(
            explicit.apps[0].config.worktree.as_ref().unwrap().label,
            "agent-a"
        );
        assert_eq!(
            workspace(&["--config", config_path.to_str().unwrap(), "--local"])
                .unwrap()
                .unwrap()
                .apps[0]
                .config
                .hostname,
            "api.agent-a.demo.localhost"
        );
        std::fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn real_linked_worktrees_have_stable_identity_from_subdirectories_and_detached_head() {
        let temp = std::env::temp_dir().join(format!("nook-worktree-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&temp).unwrap();
        let repo = temp.join("repo");
        let linked = temp.join("agent-a");
        let git = |cwd: &Path, args: &[&std::ffi::OsStr]| {
            let output = Command::new("git")
                .current_dir(cwd)
                .args(args)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
        };
        git(&temp, &["init".as_ref(), repo.as_os_str()]);
        git(
            &repo,
            &[
                "-c".as_ref(),
                "user.name=Test".as_ref(),
                "-c".as_ref(),
                "user.email=test@example.invalid".as_ref(),
                "commit".as_ref(),
                "--allow-empty".as_ref(),
                "-m".as_ref(),
                "initial".as_ref(),
            ],
        );
        git(
            &repo,
            &[
                "worktree".as_ref(),
                "add".as_ref(),
                "--detach".as_ref(),
                linked.as_os_str(),
            ],
        );
        std::fs::create_dir_all(linked.join("apps/api")).unwrap();
        let auto = Some(&Setting::Enabled(true));
        let identity = resolve(auto, &linked).unwrap().unwrap();
        assert!(identity.label.starts_with("agent-a-"));
        assert_eq!(
            resolve(auto, &linked.join("apps/api")).unwrap().unwrap(),
            identity
        );
        assert_ne!(resolve(auto, &repo).unwrap().unwrap().key, identity.key);
        git(
            &linked,
            &["switch".as_ref(), "-c".as_ref(), "another-branch".as_ref()],
        );
        assert_eq!(resolve(auto, &linked).unwrap().unwrap(), identity);
        let explicit = resolve(Some(&Setting::Named("agent-a".into())), &linked)
            .unwrap()
            .unwrap();
        assert_eq!(explicit.key, identity.key);
        assert_eq!(
            hostname("api.nook.localhost", &explicit, Some("nook")).unwrap(),
            "api.agent-a.nook.localhost"
        );
        assert_eq!(
            hostname("nook.localhost", &explicit, None).unwrap(),
            "agent-a.nook.localhost"
        );
        assert!(resolve(auto, &temp).is_err());
        assert!(resolve(None, &temp).unwrap().is_none());
        assert!(
            resolve(Some(&Setting::Enabled(false)), &temp)
                .unwrap()
                .is_none()
        );
        std::fs::remove_dir_all(temp).unwrap();
    }
}
