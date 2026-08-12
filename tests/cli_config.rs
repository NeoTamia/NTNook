use std::fs;
use std::process::{Command, Output};

use uuid::Uuid;

#[test]
fn config_commands_create_show_update_and_protect_the_global_file() {
    let directory = std::env::temp_dir().join(format!("nook-cli-config-{}", Uuid::new_v4()));
    let config_home = directory.join("config");
    let state_home = directory.join("state");

    let init = nook(
        &config_home,
        &state_home,
        &[
            "config",
            "init",
            "--caddy-socket",
            "/run/caddy/admin.socket",
        ],
    );
    assert!(
        init.status.success(),
        "{}",
        String::from_utf8_lossy(&init.stderr)
    );
    let path = config_home.join("nook/config.toml");
    assert!(
        fs::read_to_string(&path)
            .unwrap()
            .contains("unix//run/caddy/admin.socket")
    );
    let displayed_path = nook(&config_home, &state_home, &["config", "path"]);
    assert!(displayed_path.status.success());
    assert_eq!(
        String::from_utf8(displayed_path.stdout).unwrap(),
        format!("{}\n", path.display())
    );

    let repeated = nook(&config_home, &state_home, &["config", "init"]);
    assert!(!repeated.status.success());
    assert!(String::from_utf8_lossy(&repeated.stderr).contains("already exists"));

    let set = nook(
        &config_home,
        &state_home,
        &["config", "set", "run-bind-address", "127.0.0.2"],
    );
    assert!(
        set.status.success(),
        "{}",
        String::from_utf8_lossy(&set.stderr)
    );
    let show = nook(&config_home, &state_home, &["config", "show"]);
    assert!(
        show.status.success(),
        "{}",
        String::from_utf8_lossy(&show.stderr)
    );
    let shown = String::from_utf8(show.stdout).unwrap();
    assert!(shown.contains("run_bind_address = \"127.0.0.2\""));
    assert!(shown.contains("caddy_admin = \"unix//run/caddy/admin.socket\""));

    fs::remove_dir_all(directory).unwrap();
}

#[test]
fn config_show_materializes_defaults_without_changing_the_file() {
    let directory = std::env::temp_dir().join(format!("nook-cli-config-{}", Uuid::new_v4()));
    let config_home = directory.join("config");
    let state_home = directory.join("state");
    let nook_directory = config_home.join("nook");
    fs::create_dir_all(&nook_directory).unwrap();
    let contents = "format_version = 1\ncaddy_admin = \"http://127.0.0.1:2019\"\n";
    fs::write(nook_directory.join("config.toml"), contents).unwrap();

    let show = nook(&config_home, &state_home, &["config", "show"]);
    assert!(
        show.status.success(),
        "{}",
        String::from_utf8_lossy(&show.stderr)
    );
    let shown = String::from_utf8(show.stdout).unwrap();
    assert!(shown.contains("caddy_admin = \"http://127.0.0.1:2019\""));
    assert!(shown.contains("run_bind_address = \"127.0.0.1\""));
    assert!(shown.contains("caddy_loopback_host = \"127.0.0.1\""));
    assert!(shown.contains("caddy_client_ip_ranges = ["));
    assert_eq!(
        fs::read_to_string(nook_directory.join("config.toml")).unwrap(),
        contents
    );

    fs::remove_dir_all(directory).unwrap();
}

fn nook(config_home: &std::path::Path, state_home: &std::path::Path, arguments: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_nook"))
        .env("XDG_CONFIG_HOME", config_home)
        .env("XDG_STATE_HOME", state_home)
        .args(arguments)
        .output()
        .unwrap()
}
