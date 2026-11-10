use std::{fs, path::Path, process::Command};
use uuid::Uuid;
fn command(root: &Path) -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_nook"));
    c.current_dir(root)
        .env("XDG_CONFIG_HOME", root.join("config"))
        .env("XDG_STATE_HOME", root.join("state"))
        .env("APPDATA", root.join("config"))
        .env("LOCALAPPDATA", root.join("state"))
        .env("NOOK_DISABLE_UPDATE_CHECK", "1")
        .env_remove("NOOK_WORKSPACE_SESSION");
    c
}
#[test]
fn tunnel_options_templates_and_preflight_are_explicit() {
    let root = std::env::temp_dir().join(format!("nook-tunnel-cli-{}", Uuid::new_v4()));
    fs::create_dir_all(&root).unwrap();
    for flags in [vec![], vec!["--workspace"]] {
        let output = command(&root)
            .args(["init", "--print", "--name", "web", "--tunnel", "cloudflare"])
            .args(flags)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let config: toml::Value =
            toml::from_str(&String::from_utf8(output.stdout).unwrap()).unwrap();
        let entry = config.get("apps").map_or(&config, |apps| &apps[0]);
        assert_eq!(entry["tunnel"].as_str(), Some("cloudflare"));
    }
    for flags in [
        vec!["run", "--tunnel", "ngrok"],
        vec!["run", "--tunnel", "cloudflare", "--no-tunnel"],
        vec!["alias", "set", "web", "3000", "--tunnel", "cloudflare"],
    ] {
        assert!(
            !command(&root)
                .args(flags)
                .output()
                .unwrap()
                .status
                .success()
        );
    }
    #[cfg(unix)]
    let config_dir = root.join("config/nook");
    #[cfg(windows)]
    let config_dir = root.join("config/Nook");
    fs::create_dir_all(&config_dir).unwrap();
    fs::write(config_dir.join("config.toml"), "format_version=1\ncaddy_admin='http://127.0.0.1:1'\n[cloudflare]\nbinary='missing-nook-test-cloudflared'\n").unwrap();
    fs::write(
        root.join("nook.toml"),
        "format_version=1\nname='web'\ntunnel='cloudflare'\ncommand=['unused']\n",
    )
    .unwrap();
    let output = command(&root).arg("run").output().unwrap();
    assert!(String::from_utf8_lossy(&output.stderr).contains("install it explicitly"));
    // Explicit disabling gets past connector preflight to the unavailable Caddy.
    let output = command(&root)
        .args(["run", "--no-tunnel"])
        .output()
        .unwrap();
    assert!(!String::from_utf8_lossy(&output.stderr).contains("cloudflared"));
    fs::write(
        root.join("nook.local.toml"),
        "format_version=1\ntunnel='none'\n",
    )
    .unwrap();
    let output = command(&root).arg("run").output().unwrap();
    assert!(!String::from_utf8_lossy(&output.stderr).contains("cloudflared"));
    let output = command(&root)
        .args(["run", "--tunnel", "cloudflare"])
        .output()
        .unwrap();
    assert!(String::from_utf8_lossy(&output.stderr).contains("install it explicitly"));
    fs::remove_dir_all(root).unwrap();
}
