use std::fs;
use std::process::{Command, Output};

use serde_json::json;
use uuid::Uuid;

#[test]
fn completion_scripts_are_generated_without_configuration_or_caddy() {
    for (shell, markers) in [
        (
            "bash",
            &[
                "_nook()",
                "complete -F _nook",
                "--readiness-warn-after",
                "--local",
                "--print",
                "init",
                "update",
            ][..],
        ),
        (
            "zsh",
            &[
                "#compdef nook",
                "compdef _nook nook",
                "--readiness-warn-after",
                "--local",
                "--print",
                "init",
                "update",
            ][..],
        ),
        (
            "power-shell",
            &[
                "Register-ArgumentCompleter",
                "__complete $dynamicKind",
                "--readiness-warn-after",
                "--local",
                "--print",
                "init",
                "update",
            ][..],
        ),
    ] {
        let output = nook(&[
            "--caddy-socket",
            "/definitely/missing",
            "completions",
            shell,
        ]);
        assert!(
            output.status.success(),
            "{shell}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(output.stderr.is_empty());

        let script = String::from_utf8(output.stdout).unwrap();
        for marker in markers {
            assert!(
                script.contains(marker),
                "{shell} completion omitted {marker}"
            );
        }
        assert!(!script.contains("nook,__complete"));
        assert!(!script.contains("(__complete)"));
    }
}

#[test]
fn completion_help_lists_supported_shells_and_rejects_others() {
    let help = nook(&["completions", "--help"]);
    assert!(help.status.success());
    let help = String::from_utf8(help.stdout).unwrap();
    assert!(help.contains("completions <SHELL>"));
    assert!(help.contains("[possible values: bash, zsh, power-shell]"));

    let rejected = nook(&["completions", "fish"]);
    assert!(!rejected.status.success());
    assert!(String::from_utf8_lossy(&rejected.stderr).contains("invalid value 'fish'"));
}

#[test]
#[cfg(unix)]
fn generated_bash_completion_has_valid_syntax() {
    let output = nook(&["completions", "bash"]);
    assert!(output.status.success());

    let directory = std::env::temp_dir().join(format!("nook-cli-completions-{}", Uuid::new_v4()));
    fs::create_dir(&directory).unwrap();
    let path = directory.join("nook.bash");
    fs::write(&path, output.stdout).unwrap();

    let syntax = Command::new("bash")
        .args(["-n"])
        .arg(&path)
        .output()
        .unwrap();
    assert!(
        syntax.status.success(),
        "{}",
        String::from_utf8_lossy(&syntax.stderr)
    );
    fs::remove_dir_all(directory).unwrap();
}

#[test]
#[cfg(unix)]
fn generated_zsh_completion_has_valid_syntax() {
    if Command::new("zsh").arg("--version").output().is_err() {
        return;
    }

    let output = nook(&["completions", "zsh"]);
    assert!(output.status.success());

    let directory = std::env::temp_dir().join(format!("nook-cli-completions-{}", Uuid::new_v4()));
    fs::create_dir(&directory).unwrap();
    let path = directory.join("_nook");
    fs::write(&path, output.stdout).unwrap();

    let syntax = Command::new("zsh")
        .args(["-n"])
        .arg(&path)
        .output()
        .unwrap();
    assert!(
        syntax.status.success(),
        "{}",
        String::from_utf8_lossy(&syntax.stderr)
    );
    fs::remove_dir_all(directory).unwrap();
}

#[test]
#[cfg(windows)]
fn powershell_completion_proposes_dynamic_names() {
    let directory = temporary_directory("dynamic-powershell");
    let state_home = directory.join("state");
    let state_directory = state_home.join("nook");
    let bin_directory = directory.join("bin");
    fs::create_dir_all(&state_directory).unwrap();
    fs::create_dir(&bin_directory).unwrap();
    write_registry(&state_directory.join("state.json"));
    fs::copy(env!("CARGO_BIN_EXE_nook"), bin_directory.join("nook.exe")).unwrap();

    let script_path = directory.join("nook.ps1");
    let script = Command::new(env!("CARGO_BIN_EXE_nook"))
        .env("NOOK_DISABLE_UPDATE_CHECK", "1")
        .args(["completions", "power-shell"])
        .output()
        .unwrap();
    assert!(script.status.success());
    fs::write(&script_path, script.stdout).unwrap();

    let path = std::env::var_os("PATH").unwrap();
    let path = format!("{};{}", bin_directory.display(), path.to_string_lossy());
    for (line, expected) in [
        ("nook stop a", "api"),
        ("nook stop A", "api"),
        ("nook stop --force a", "api"),
        ("nook alias remove d", "docs"),
        ("nook alias d", "docs"),
    ] {
        let output = Command::new("powershell.exe")
            .args([
                "-NoLogo",
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                ". $env:NOOK_COMPLETION_SCRIPT; (TabExpansion2 $env:NOOK_COMPLETION_LINE $env:NOOK_COMPLETION_CURSOR).CompletionMatches.CompletionText",
            ])
            .env("PATH", &path)
            .env("XDG_STATE_HOME", &state_home)
            .env("NOOK_DISABLE_UPDATE_CHECK", "1")
            .env("NOOK_COMPLETION_SCRIPT", &script_path)
            .env("NOOK_COMPLETION_LINE", line)
            .env("NOOK_COMPLETION_CURSOR", line.len().to_string())
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), expected);
    }

    fs::remove_dir_all(directory).unwrap();
}

#[test]
fn dynamic_completion_reads_runs_and_aliases_without_caddy() {
    let directory = temporary_directory("dynamic");
    let state_home = directory.join("state");
    let state_directory = state_home.join("nook");
    fs::create_dir_all(&state_directory).unwrap();
    write_registry(&state_directory.join("state.json"));

    for (kind, expected) in [("runs", "api\n"), ("aliases", "docs\n")] {
        let output = Command::new(env!("CARGO_BIN_EXE_nook"))
            .env("XDG_STATE_HOME", &state_home)
            .env("NOOK_DISABLE_UPDATE_CHECK", "1")
            .args(["__complete", kind])
            .output()
            .unwrap();
        assert!(output.status.success());
        assert_eq!(String::from_utf8(output.stdout).unwrap(), expected);
        assert!(output.stderr.is_empty());
    }

    fs::remove_dir_all(directory).unwrap();
}

#[test]
fn dynamic_completion_returns_empty_for_missing_invalid_or_locked_state() {
    let directory = temporary_directory("dynamic-empty");
    let state_home = directory.join("state");
    let state_directory = state_home.join("nook");
    let state_path = state_directory.join("state.json");
    let lock_path = state_directory.join("state.lock");

    let missing = run_completion(&state_home, "runs");
    assert!(missing.status.success());
    assert!(missing.stdout.is_empty());
    assert!(!state_home.exists());

    fs::create_dir_all(&state_directory).unwrap();
    fs::write(&state_path, b"not json").unwrap();
    let invalid = run_completion(&state_home, "runs");
    assert!(invalid.status.success());
    assert!(invalid.stdout.is_empty());

    write_registry(&state_path);
    let lock = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&lock_path)
        .unwrap();
    lock.lock().unwrap();

    let locked = run_completion(&state_home, "runs");
    assert!(locked.status.success());
    assert!(locked.stdout.is_empty());
    drop(lock);

    fs::remove_dir_all(directory).unwrap();
}

#[test]
#[cfg(unix)]
fn bash_completion_proposes_dynamic_names_for_stop_and_alias_remove() {
    let directory = temporary_directory("dynamic-bash");
    let state_home = directory.join("state");
    let state_directory = state_home.join("nook");
    let bin_directory = directory.join("bin");
    fs::create_dir_all(&state_directory).unwrap();
    fs::create_dir(&bin_directory).unwrap();
    write_registry(&state_directory.join("state.json"));
    std::os::unix::fs::symlink(env!("CARGO_BIN_EXE_nook"), bin_directory.join("nook")).unwrap();

    let script_path = directory.join("nook.bash");
    let script = Command::new(env!("CARGO_BIN_EXE_nook"))
        .env("NOOK_DISABLE_UPDATE_CHECK", "1")
        .args(["completions", "bash"])
        .output()
        .unwrap();
    assert!(script.status.success());
    fs::write(&script_path, script.stdout).unwrap();

    let path = std::env::var_os("PATH").unwrap();
    let path = format!("{}:{}", bin_directory.display(), path.to_string_lossy());
    for (words, cword, expected) in [
        ("nook stop a", "2", "api\n"),
        ("nook stop --force a", "3", "api\n"),
        (
            "nook stop --caddy-socket /tmp/caddy.sock --force a",
            "5",
            "api\n",
        ),
        (
            "nook --caddy-socket /tmp/caddy.sock stop --force a",
            "5",
            "api\n",
        ),
        ("nook alias remove d", "3", "docs\n"),
        (
            "nook alias remove --caddy-socket /tmp/caddy.sock d",
            "5",
            "docs\n",
        ),
        (
            "nook alias --caddy-socket /tmp/caddy.sock remove d",
            "5",
            "docs\n",
        ),
        (
            "nook --caddy-socket /tmp/caddy.sock alias remove d",
            "5",
            "docs\n",
        ),
        ("nook a", "1", "alias\napi\n"),
        ("nook alias d", "2", "docs\n"),
        ("nook alias --caddy-socket /tmp/caddy.sock d", "4", "docs\n"),
    ] {
        let output = Command::new("bash")
            .env("PATH", &path)
            .env("XDG_STATE_HOME", &state_home)
            .env("NOOK_DISABLE_UPDATE_CHECK", "1")
            .args([
                "-c",
                "source \"$1\"; COMP_WORDS=($2); COMP_CWORD=$3; _nook_dynamic nook \"${COMP_WORDS[COMP_CWORD]}\" \"${COMP_WORDS[COMP_CWORD-1]}\"; printf '%s\\n' \"${COMPREPLY[@]}\"",
                "bash",
                script_path.to_str().unwrap(),
                words,
                cword,
            ])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(String::from_utf8(output.stdout).unwrap(), expected);
    }

    fs::remove_dir_all(directory).unwrap();
}

fn nook(arguments: &[&str]) -> Output {
    let directory = std::env::temp_dir().join(format!("nook-completions-env-{}", Uuid::new_v4()));
    let config_home = directory.join("config");
    let config_directory = config_home.join("nook");
    fs::create_dir_all(&config_directory).unwrap();
    fs::write(config_directory.join("config.toml"), "invalid = [").unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_nook"))
        .env("XDG_CONFIG_HOME", config_home)
        .env("XDG_STATE_HOME", directory.join("state"))
        .env("NOOK_DISABLE_UPDATE_CHECK", "1")
        .args(arguments)
        .output()
        .unwrap();
    fs::remove_dir_all(directory).unwrap();
    output
}

fn run_completion(state_home: &std::path::Path, kind: &str) -> Output {
    Command::new(env!("CARGO_BIN_EXE_nook"))
        .env("XDG_STATE_HOME", state_home)
        .env("NOOK_DISABLE_UPDATE_CHECK", "1")
        .args(["__complete", kind])
        .output()
        .unwrap()
}

fn write_registry(path: &std::path::Path) {
    let lease_id = Uuid::new_v4();
    let alias_id = Uuid::new_v4();
    let contents = json!({
        "format_version": 1,
        "aliases": {
            "docs.localhost": {
                "id": alias_id,
                "hostname": "docs.localhost",
                "target": "http://127.0.0.1:4173",
                "scheme": "http",
                "tls": true,
                "preserve_host": false
            }
        },
        "leases": {
            lease_id.to_string(): {
                "id": lease_id,
                "hostname": "api.localhost",
                "target": "http://127.0.0.1:3000",
                "scheme": "http",
                "tls": true,
                "pid": 1234,
                "pgid": 1234,
                "process_start_time_ticks": 1,
                "state": "ready"
            }
        },
        "selected_servers": {
            "https": null,
            "http": null
        },
        "last_synchronized_at_unix_ms": null,
        "pending_operations": []
    });
    fs::write(path, serde_json::to_vec(&contents).unwrap()).unwrap();
}

fn temporary_directory(label: &str) -> std::path::PathBuf {
    let path = std::env::temp_dir().join(format!("nook-completions-{label}-{}", Uuid::new_v4()));
    fs::create_dir(&path).unwrap();
    path
}
