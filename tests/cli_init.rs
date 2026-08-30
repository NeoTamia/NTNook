use std::fs;
use std::process::{Command, Output};

use uuid::Uuid;

#[test]
fn init_creates_a_documented_project_configuration() {
    let directory = temporary_directory();
    let output = nook(
        &directory,
        &[
            "init",
            "--name",
            "API.localhost",
            "--no-tls",
            "--app-port",
            "5173",
            "--strict-port",
            "--readiness-warn-after",
            "12",
            "--",
            "pnpm",
            "run",
            "dev",
        ],
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let contents = fs::read_to_string(directory.join("nook.toml")).unwrap();
    assert!(contents.contains("format_version = 1"));
    assert!(contents.contains("name = \"api\""));
    assert!(contents.contains("command = [\"pnpm\", \"run\", \"dev\"]"));
    assert!(contents.contains("tls = false"));
    assert!(contents.contains("app_port = 5173"));
    assert!(contents.contains("strict_port = true"));
    assert!(contents.contains("readiness_warn_after_seconds = 12"));

    toml::from_str::<toml::Value>(&contents).expect("generated TOML should parse");
    fs::remove_dir_all(directory).unwrap();
}

#[test]
fn init_prints_without_writing_and_protects_existing_files() {
    let directory = temporary_directory();
    let printed = nook(&directory, &["init", "--print", "--name", "demo"]);
    assert!(printed.status.success());
    assert!(
        String::from_utf8(printed.stdout)
            .unwrap()
            .contains("name = \"demo\"")
    );
    assert!(!directory.join("nook.toml").exists());

    fs::write(directory.join("nook.toml"), "keep me").unwrap();
    let refused = nook(&directory, &["init", "--name", "demo"]);
    assert!(!refused.status.success());
    assert!(String::from_utf8_lossy(&refused.stderr).contains("use --force"));
    assert_eq!(
        fs::read_to_string(directory.join("nook.toml")).unwrap(),
        "keep me"
    );

    let replaced = nook(&directory, &["init", "--force", "--name", "demo"]);
    assert!(replaced.status.success());
    assert!(
        fs::read_to_string(directory.join("nook.toml"))
            .unwrap()
            .contains("name = \"demo\"")
    );
    fs::remove_dir_all(directory).unwrap();
}

#[test]
fn local_init_creates_only_local_overrides() {
    let directory = temporary_directory();
    let output = nook(&directory, &["init", "--local", "--app-port", "5180"]);
    assert!(output.status.success());
    assert!(!directory.join("nook.toml").exists());
    let contents = fs::read_to_string(directory.join("nook.local.toml")).unwrap();
    assert!(contents.contains("# name ="));
    assert!(contents.contains("app_port = 5180"));
    assert!(!contents.contains("\nname ="));
    fs::remove_dir_all(directory).unwrap();
}

fn nook(directory: &std::path::Path, arguments: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_nook"))
        .current_dir(directory)
        .env("NOOK_DISABLE_UPDATE_CHECK", "1")
        .args(arguments)
        .output()
        .unwrap()
}

fn temporary_directory() -> std::path::PathBuf {
    let directory = std::env::temp_dir().join(format!("nook-cli-init-{}", Uuid::new_v4()));
    fs::create_dir(&directory).unwrap();
    directory
}
