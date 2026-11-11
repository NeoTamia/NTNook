//! Portable CLI boundary proof. A copy of this binary plays the Docker executable.
use serde_json::{Value, json};
use std::{env, fs, io::Write, path::PathBuf, process::Command};

fn main() {
    if env::current_exe().unwrap().file_stem().unwrap() == "docker" {
        fake_docker();
        return;
    }
    let root = env::temp_dir().join(format!("nook compose cli {}", uuid::Uuid::new_v4()));
    fs::create_dir_all(&root).unwrap();
    let docker = root.join(if cfg!(windows) {
        "docker.exe"
    } else {
        "docker"
    });
    fs::copy(env::current_exe().unwrap(), docker).unwrap();
    // On Windows, Rust searches the executable's directory before System32,
    // but an inherited PATH comes after System32 (which may contain Docker).
    // Keep Nook beside the fake so this test never reaches the installed daemon.
    let nook_binary = root.join(if cfg!(windows) { "nook.exe" } else { "nook" });
    fs::copy(env!("CARGO_BIN_EXE_nook"), &nook_binary).unwrap();
    fs::write(root.join("compose.yaml"), "services: {}\n").unwrap();
    fs::write(root.join("local env"), "VALUE=private\n").unwrap();
    let model = json!({"name":"app","services":{"web":{"ports":[{"target":8080,"protocol":"udp"}],"labels":{"io.ntnook.route.http.port":"8080"}}}});
    fs::write(root.join("model.json"), model.to_string()).unwrap();
    let path = env::join_paths(
        std::iter::once(root.clone())
            .chain(env::split_paths(&env::var_os("PATH").unwrap_or_default())),
    )
    .unwrap();
    let nook = |args: &[&str]| {
        Command::new(&nook_binary)
            .args(args)
            .current_dir(&root)
            .env("PATH", &path)
            .env("NOOK_FAKE_DOCKER_ROOT", &root)
            .env("NOOK_DISABLE_UPDATE_CHECK", "1")
            .env_remove("DOCKER_HOST")
            .env_remove("DOCKER_CONTEXT")
            .env_remove("COMPOSE_FILE")
            .env_remove("COMPOSE_PROJECT_NAME")
            .env_remove("COMPOSE_PROFILES")
            .env("XDG_CONFIG_HOME", root.join("config"))
            .env("XDG_STATE_HOME", root.join("state"))
            .env("APPDATA", root.join("config"))
            .env("LOCALAPPDATA", root.join("state"))
            .output()
            .unwrap()
    };
    let help = nook(&["compose", "--help"]);
    assert!(help.status.success());
    assert!(String::from_utf8_lossy(&help.stdout).contains("down"));
    assert!(
        !root.join("calls.jsonl").exists(),
        "help must not discover Docker"
    );
    for args in [
        vec!["compose", "down", "--volumes"],
        vec!["compose", "up", "--remove-orphans"],
        vec!["compose", "exec", "web", "sh"],
    ] {
        assert_eq!(nook(&args).status.code(), Some(2));
    }
    assert!(
        !root.join("calls.jsonl").exists(),
        "unsupported passthrough must not run Docker"
    );
    // Normalize actual inputs before rejecting a non-HTTP publication. No Caddy required.
    let rejected = nook(&[
        "compose",
        "-f",
        "compose.yaml",
        "--env-file",
        "local env",
        "-p",
        "explicit",
        "--no-worktree",
        "up",
        "-d",
    ]);
    assert!(!rejected.status.success());
    assert!(
        String::from_utf8_lossy(&rejected.stderr).contains("HTTP over TCP"),
        "{}\nFake Docker calls: {}",
        String::from_utf8_lossy(&rejected.stderr),
        fs::read_to_string(root.join("calls.jsonl")).unwrap_or_else(|_| "<none>".into())
    );
    let calls: Vec<Vec<String>> = fs::read_to_string(root.join("calls.jsonl"))
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert!(
        calls
            .iter()
            .any(|a| a.windows(2).any(|w| w == ["-p", "explicit"]))
    );
    assert!(
        calls
            .iter()
            .any(|a| a.iter().any(|v| v.ends_with("local env")))
    );
    assert!(
        !calls
            .iter()
            .any(|a| a.iter().any(|v| v == "up" || v == "stop" || v == "rm"))
    );
    assert!(!root.join("state/nook/state.json").exists());
    let config = nook(&["compose", "-p", "explicit", "config"]);
    assert!(config.status.success());
    assert!(String::from_utf8_lossy(&config.stdout).contains("explicit"));
    fs::remove_dir_all(&root).unwrap();
    println!(
        "Compose CLI: help, bounded passthrough, normalization, argument fidelity and pre-mutation validation passed"
    );
}

fn fake_docker() {
    let root = PathBuf::from(env::var_os("NOOK_FAKE_DOCKER_ROOT").unwrap());
    let args: Vec<String> = env::args().skip(1).collect();
    let mut calls = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(root.join("calls.jsonl"))
        .unwrap();
    writeln!(calls, "{}", serde_json::to_string(&args).unwrap()).unwrap();
    let value = match args.first().map(String::as_str) {
        Some("context") => {
            json!([{"Endpoints":{"docker":{"Host":if cfg!(windows){"npipe:////./pipe/docker_engine"}else{"unix:///fake/docker.sock"}}}}])
        }
        Some("info") => json!({"ID":"test-engine","OSType":"linux"}),
        Some("compose") if args.iter().any(|v| v == "config") => {
            let mut model: Value =
                serde_json::from_slice(&fs::read(root.join("model.json")).unwrap()).unwrap();
            if let Some(i) = args.iter().position(|v| v == "-p") {
                model["name"] = json!(args[i + 1]);
            }
            model
        }
        _ => {
            eprintln!("unexpected Docker invocation");
            std::process::exit(99);
        }
    };
    println!("{value}");
}
