//! Session detection must fail closed before ordinary config/Caddy processing.
use std::fs;
use std::path::Path;
use std::process::{Command, Output};

fn session_run(directory: &Path, marker: &std::ffi::OsStr, arguments: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_nook"))
        .current_dir(directory)
        .env("XDG_CONFIG_HOME", directory.join("config"))
        .env("XDG_STATE_HOME", directory.join("state"))
        .env("NOOK_DISABLE_UPDATE_CHECK", "1")
        .env("NOOK_WORKSPACE_SESSION", marker)
        .args(arguments)
        .output()
        .unwrap()
}

#[test]
fn supplied_invalid_session_never_becomes_a_standalone_run() {
    let directory =
        std::env::temp_dir().join(format!("nook-invalid-session-{}", uuid::Uuid::new_v4()));
    fs::create_dir_all(directory.join("config/nook")).unwrap();
    // A member gets its config from the session, even if global config is broken.
    fs::write(directory.join("config/nook/config.toml"), "invalid = [").unwrap();
    for marker in [
        std::ffi::OsString::new(),
        directory.join("missing/context.json").into_os_string(),
        std::ffi::OsString::from("relative/context.json"),
    ] {
        let output = session_run(&directory, &marker, &["run", "--", "must-never-start"]);
        assert!(!output.status.success());
        let error = String::from_utf8_lossy(&output.stderr);
        assert!(error.contains("workspace session"), "{error}");
        assert!(!error.contains("invalid configuration"), "{error}");
        assert!(!directory.join("state/nook/state.json").exists());
    }
    fs::remove_dir_all(directory).unwrap();
}

#[test]
fn external_runner_requires_a_workspace_and_its_own_command() {
    let directory = std::env::temp_dir().join(format!("nook-runner-cli-{}", uuid::Uuid::new_v4()));
    fs::create_dir(&directory).unwrap();
    for arguments in [
        vec!["run", "--runner", "--", "must-never-start"],
        vec!["run", "--runner"],
        vec!["run", "--runner", "--only", "api", "--", "must-never-start"],
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_nook"))
            .current_dir(&directory)
            .env("XDG_CONFIG_HOME", directory.join("config"))
            .env("XDG_STATE_HOME", directory.join("state"))
            .env("NOOK_DISABLE_UPDATE_CHECK", "1")
            .env_remove("NOOK_WORKSPACE_SESSION")
            .args(arguments)
            .output()
            .unwrap();
        assert!(!output.status.success());
        let error = String::from_utf8_lossy(&output.stderr);
        assert!(error.contains("--runner"), "{error}");
        assert!(!directory.join("state/nook/state.json").exists());
    }
    fs::remove_dir_all(directory).unwrap();
}
