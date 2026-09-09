#![cfg(unix)]

use std::fs;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::process::{Command, Output};
use std::sync::{Arc, Mutex};
use std::thread;

use serde_json::{Value, json};

#[test]
fn help_is_successful_and_never_reported_as_an_error() {
    let output = Command::new(env!("CARGO_BIN_EXE_nook"))
        .env("NOOK_DISABLE_UPDATE_CHECK", "1")
        .arg("--help")
        .output()
        .unwrap();
    assert_success(&output);
    assert!(output.stderr.is_empty());
    assert!(String::from_utf8_lossy(&output.stdout).contains("Usage: nook [OPTIONS] <COMMAND>"));
}

#[test]
fn alias_shortcuts_persist_list_and_remove_idempotently() {
    let directory = std::env::temp_dir().join(format!(
        "nook-cli-alias-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let config_home = directory.join("config");
    let state_home = directory.join("state");
    fs::create_dir_all(config_home.join("nook")).unwrap();

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let admin = format!("http://{}", listener.local_addr().unwrap());
    fs::write(
        config_home.join("nook/config.toml"),
        format!("format_version = 1\ncaddy_admin = \"{admin}\"\n"),
    )
    .unwrap();

    let routes = Arc::new(Mutex::new(Vec::<Value>::new()));
    let server_routes = Arc::clone(&routes);
    let server = thread::spawn(move || serve_caddy(listener, server_routes, 25));

    let set = nook(&config_home, &state_home, &["alias", "api", "3000"]);
    assert_success(&set);
    assert_eq!(
        String::from_utf8(set.stdout).unwrap(),
        "api.localhost -> http://127.0.0.1:3000/\n"
    );

    let list = nook(&config_home, &state_home, &["alias", "list"]);
    assert_success(&list);
    assert_eq!(
        String::from_utf8(list.stdout).unwrap(),
        "api.localhost -> http://127.0.0.1:3000/\n"
    );

    let managed = nook(&config_home, &state_home, &["list"]);
    assert_success(&managed);
    assert_eq!(
        String::from_utf8(managed.stdout).unwrap(),
        "alias\tpersistent\tapi.localhost\thttp://127.0.0.1:3000/\n"
    );

    let status = nook(&config_home, &state_home, &["status"]);
    assert_success(&status);
    let status_output = String::from_utf8(status.stdout).unwrap();
    assert!(status_output.contains("caddy\tok\n"));
    assert!(status_output.contains("https_container\tpresent\n"));
    assert!(status_output.contains("drift\tclean\n"));
    assert!(status_output.contains("local_ca\tnot trusted\n"));
    assert!(String::from_utf8_lossy(&status.stderr).contains("caddy trust --address"));

    let prune = nook(&config_home, &state_home, &["prune"]);
    assert_success(&prune);
    assert!(
        String::from_utf8(prune.stdout)
            .unwrap()
            .contains("restored=1")
    );

    let remove = nook(&config_home, &state_home, &["alias", "--remove", "api"]);
    assert_success(&remove);
    assert_eq!(
        String::from_utf8(remove.stdout).unwrap(),
        "removed api.localhost\n"
    );

    let repeated = nook(&config_home, &state_home, &["alias", "remove", "api"]);
    assert_success(&repeated);
    assert_eq!(
        String::from_utf8(repeated.stdout).unwrap(),
        "alias api.localhost is not configured\n"
    );

    server.join().unwrap();
    assert!(routes.lock().unwrap().is_empty());
    fs::remove_dir_all(directory).unwrap();
}

#[test]
fn status_has_a_stable_failure_when_caddy_is_unavailable() {
    let directory = std::env::temp_dir().join(format!(
        "nook-cli-status-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let config_home = directory.join("config");
    let state_home = directory.join("state");
    fs::create_dir_all(config_home.join("nook")).unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let admin = format!("http://{}", listener.local_addr().unwrap());
    drop(listener);
    fs::write(
        config_home.join("nook/config.toml"),
        format!("format_version = 1\ncaddy_admin = \"{admin}\"\n"),
    )
    .unwrap();

    let status = nook(&config_home, &state_home, &["status"]);
    assert_eq!(status.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&status.stderr).contains("Caddy Admin API request failed"));
    fs::remove_dir_all(directory).unwrap();
}

#[test]
fn run_preserves_child_exit_when_cleanup_becomes_unavailable() {
    let (directory, config_home, state_home) = temporary_homes("run-cleanup");
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    write_global_config(&config_home, listener.local_addr().unwrap());
    let routes = Arc::new(Mutex::new(Vec::<Value>::new()));
    let server_routes = Arc::clone(&routes);
    let server = thread::spawn(move || serve_caddy(listener, server_routes, 4));
    let script = "import os,socket,time;s=socket.socket();s.bind(('127.0.0.1',int(os.environ['PORT'])));s.listen();time.sleep(.1);raise SystemExit(7)";
    let run = nook(
        &config_home,
        &state_home,
        &[
            "run",
            "--name",
            "child",
            "--",
            "/usr/bin/python3",
            "-c",
            script,
        ],
    );
    server.join().unwrap();
    assert_eq!(run.status.code(), Some(7));
    assert!(String::from_utf8_lossy(&run.stderr).contains("cleanup of child.localhost is pending"));
    let state = fs::read_to_string(state_home.join("nook/state.json")).unwrap();
    assert!(state.contains("remove_route"));
    fs::remove_dir_all(directory).unwrap();
}

#[test]
fn run_reports_inferred_domain_url_and_effective_port() {
    let (directory, config_home, state_home) = temporary_homes("run-info");
    let project = directory.join("inferred-app");
    fs::create_dir_all(&project).unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    write_global_config(&config_home, listener.local_addr().unwrap());
    let routes = Arc::new(Mutex::new(Vec::<Value>::new()));
    let server_routes = Arc::clone(&routes);
    let server = thread::spawn(move || serve_caddy(listener, server_routes, 6));
    let script = "import os,socket,time;s=socket.socket();s.bind(('127.0.0.1',int(os.environ['PORT'])));s.listen();time.sleep(.1)";
    let run = Command::new(env!("CARGO_BIN_EXE_nook"))
        .env("NOOK_DISABLE_UPDATE_CHECK", "1")
        .args(["run", "--", "/usr/bin/python3", "-c", script])
        .current_dir(&project)
        .env("XDG_CONFIG_HOME", &config_home)
        .env("XDG_STATE_HOME", &state_home)
        .output()
        .unwrap();
    server.join().unwrap();
    assert_success(&run);
    let stderr = String::from_utf8(run.stderr).unwrap();
    let info = stderr
        .lines()
        .find(|line| line.starts_with("nook: "))
        .expect("run information must be printed");
    assert!(info.contains("domain=inferred-app.localhost"));
    assert!(info.contains("url=https://inferred-app.localhost"));
    let port = info
        .split("port=")
        .nth(1)
        .and_then(|value| value.parse::<u16>().ok())
        .expect("effective port must be printed as a u16");
    assert_ne!(port, 0);
    assert!(routes.lock().unwrap().is_empty());
    fs::remove_dir_all(directory).unwrap();
}

#[test]
fn caddy_failure_before_run_never_starts_the_child() {
    let (directory, config_home, state_home) = temporary_homes("run-preflight");
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    drop(listener);
    write_global_config(&config_home, address);
    let marker = directory.join("child-started");
    let run = nook(
        &config_home,
        &state_home,
        &[
            "run",
            "--name",
            "child",
            "--",
            "/bin/sh",
            "-c",
            &format!("touch {}", marker.display()),
        ],
    );
    assert_eq!(run.status.code(), Some(1));
    assert!(!marker.exists());
    assert!(String::from_utf8_lossy(&run.stderr).contains("Caddy Admin API request failed"));
    fs::remove_dir_all(directory).unwrap();
}

#[test]
fn stop_command_targets_the_current_managed_process_group() {
    let (directory, config_home, state_home) = temporary_homes("stop");
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    write_global_config(&config_home, listener.local_addr().unwrap());
    let routes = Arc::new(Mutex::new(Vec::<Value>::new()));
    let server_routes = Arc::clone(&routes);
    let server = thread::spawn(move || serve_caddy(listener, server_routes, 9));
    let mut running = Command::new(env!("CARGO_BIN_EXE_nook"))
        .env("NOOK_DISABLE_UPDATE_CHECK", "1")
        .args(["run", "--name", "sleeper", "--", "/bin/sleep", "10"])
        .env("XDG_CONFIG_HOME", &config_home)
        .env("XDG_STATE_HOME", &state_home)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let state_path = state_home.join("nook/state.json");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    while std::time::Instant::now() < deadline {
        if fs::read_to_string(&state_path).is_ok_and(|state| state.contains("sleeper.localhost")) {
            break;
        }
        thread::sleep(std::time::Duration::from_millis(20));
    }
    assert!(
        fs::read_to_string(&state_path)
            .unwrap()
            .contains("sleeper.localhost")
    );
    let stop = nook(&config_home, &state_home, &["stop", "sleeper"]);
    assert_success(&stop);
    assert_eq!(
        String::from_utf8(stop.stdout).unwrap(),
        "sent SIGTERM to sleeper.localhost\n"
    );
    let outcome = running.wait().unwrap();
    server.join().unwrap();
    assert_eq!(outcome.code(), Some(143));
    assert!(routes.lock().unwrap().is_empty());
    fs::remove_dir_all(directory).unwrap();
}

#[test]
fn sigint_is_forwarded_and_the_route_is_cleaned_up() {
    let (directory, config_home, state_home) = temporary_homes("sigint");
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    write_global_config(&config_home, listener.local_addr().unwrap());
    let routes = Arc::new(Mutex::new(Vec::<Value>::new()));
    let server_routes = Arc::clone(&routes);
    let server = thread::spawn(move || serve_caddy(listener, server_routes, 6));
    let script = "import os,socket,time;s=socket.socket();s.setsockopt(socket.SOL_SOCKET,socket.SO_REUSEADDR,1);s.bind(('127.0.0.1',int(os.environ['PORT'])));s.listen();time.sleep(10)";
    let mut running = Command::new(env!("CARGO_BIN_EXE_nook"))
        .env("NOOK_DISABLE_UPDATE_CHECK", "1")
        .args([
            "run",
            "--name",
            "interrupt",
            "--",
            "/usr/bin/python3",
            "-c",
            script,
        ])
        .env("XDG_CONFIG_HOME", &config_home)
        .env("XDG_STATE_HOME", &state_home)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    wait_for_lease(&state_home, "interrupt.localhost");
    let signal = Command::new("kill")
        .args(["-INT", &running.id().to_string()])
        .output()
        .unwrap();
    assert_success(&signal);
    let outcome = running.wait().unwrap();
    server.join().unwrap();
    assert_eq!(outcome.code(), Some(130));
    assert!(routes.lock().unwrap().is_empty());
    fs::remove_dir_all(directory).unwrap();
}

#[test]
fn sigint_during_starting_is_forwarded_and_cleaned_up() {
    let (directory, config_home, state_home) = temporary_homes("sigint-starting");
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    write_global_config(&config_home, listener.local_addr().unwrap());
    let routes = Arc::new(Mutex::new(Vec::<Value>::new()));
    let server_routes = Arc::clone(&routes);
    let server = thread::spawn(move || serve_caddy(listener, server_routes, 6));
    let mut running = Command::new(env!("CARGO_BIN_EXE_nook"))
        .env("NOOK_DISABLE_UPDATE_CHECK", "1")
        .args(["run", "--name", "starting", "--", "/bin/sleep", "10"])
        .env("XDG_CONFIG_HOME", &config_home)
        .env("XDG_STATE_HOME", &state_home)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    wait_for_lease_state(&state_home, "starting.localhost", "starting");
    let signal = Command::new("kill")
        .args(["-INT", &running.id().to_string()])
        .output()
        .unwrap();
    assert_success(&signal);
    let outcome = running.wait().unwrap();
    server.join().unwrap();
    assert_eq!(outcome.code(), Some(130));
    assert!(routes.lock().unwrap().is_empty());
    assert!(
        !fs::read_to_string(state_home.join("nook/state.json"))
            .unwrap()
            .contains("starting.localhost")
    );
    fs::remove_dir_all(directory).unwrap();
}

#[test]
fn force_stop_kills_a_group_that_ignores_sigterm() {
    let (directory, config_home, state_home) = temporary_homes("force-stop");
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    write_global_config(&config_home, listener.local_addr().unwrap());
    let routes = Arc::new(Mutex::new(Vec::<Value>::new()));
    let server_routes = Arc::clone(&routes);
    let server = thread::spawn(move || serve_caddy(listener, server_routes, 9));
    let script = "import os,signal,socket,time;signal.signal(signal.SIGTERM,signal.SIG_IGN);s=socket.socket();s.setsockopt(socket.SOL_SOCKET,socket.SO_REUSEADDR,1);s.bind(('127.0.0.1',int(os.environ['PORT'])));s.listen();time.sleep(10)";
    let mut running = Command::new(env!("CARGO_BIN_EXE_nook"))
        .env("NOOK_DISABLE_UPDATE_CHECK", "1")
        .args([
            "run",
            "--name",
            "stubborn",
            "--",
            "/usr/bin/python3",
            "-c",
            script,
        ])
        .env("XDG_CONFIG_HOME", &config_home)
        .env("XDG_STATE_HOME", &state_home)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    wait_for_lease(&state_home, "stubborn.localhost");
    let stop = nook(&config_home, &state_home, &["stop", "stubborn", "--force"]);
    assert_success(&stop);
    let outcome = running.wait().unwrap();
    server.join().unwrap();
    assert_eq!(outcome.code(), Some(137));
    assert!(routes.lock().unwrap().is_empty());
    fs::remove_dir_all(directory).unwrap();
}

#[test]
fn prune_recovers_after_the_supervising_cli_is_killed() {
    let (directory, config_home, state_home) = temporary_homes("crash-prune");
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    write_global_config(&config_home, listener.local_addr().unwrap());
    let routes = Arc::new(Mutex::new(Vec::<Value>::new()));
    let server_routes = Arc::clone(&routes);
    let server = thread::spawn(move || serve_caddy(listener, server_routes, 7));
    let script = "import os,socket,time;s=socket.socket();s.setsockopt(socket.SOL_SOCKET,socket.SO_REUSEADDR,1);s.bind(('127.0.0.1',int(os.environ['PORT'])));s.listen();time.sleep(10)";
    let mut running = Command::new(env!("CARGO_BIN_EXE_nook"))
        .env("NOOK_DISABLE_UPDATE_CHECK", "1")
        .args([
            "run",
            "--name",
            "crashed",
            "--",
            "/usr/bin/python3",
            "-c",
            script,
        ])
        .env("XDG_CONFIG_HOME", &config_home)
        .env("XDG_STATE_HOME", &state_home)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    wait_for_lease(&state_home, "crashed.localhost");
    running.kill().unwrap();
    running.wait().unwrap();

    let prune = nook(&config_home, &state_home, &["prune"]);
    assert_success(&prune);
    assert!(
        String::from_utf8(prune.stdout)
            .unwrap()
            .contains("removed_dead=1")
    );
    server.join().unwrap();
    assert!(routes.lock().unwrap().is_empty());
    fs::remove_dir_all(directory).unwrap();
}

#[test]
fn external_reload_is_reconciled_on_prune() {
    let (directory, config_home, state_home) = temporary_homes("reload");
    let routes = Arc::new(Mutex::new(Vec::<Value>::new()));
    let first_listener = TcpListener::bind("127.0.0.1:0").unwrap();
    write_global_config(&config_home, first_listener.local_addr().unwrap());
    let first_routes = Arc::clone(&routes);
    let first_server = thread::spawn(move || serve_caddy(first_listener, first_routes, 4));
    assert_success(&nook(&config_home, &state_home, &["alias", "api", "3000"]));
    first_server.join().unwrap();
    routes.lock().unwrap().clear();

    let second_listener = TcpListener::bind("127.0.0.1:0").unwrap();
    write_global_config(&config_home, second_listener.local_addr().unwrap());
    let second_routes = Arc::clone(&routes);
    let second_server = thread::spawn(move || serve_caddy(second_listener, second_routes, 3));
    let prune = nook(&config_home, &state_home, &["prune"]);
    assert_success(&prune);
    assert!(
        String::from_utf8(prune.stdout)
            .unwrap()
            .contains("restored=1")
    );
    second_server.join().unwrap();
    assert_eq!(
        routes.lock().unwrap()[0]
            .pointer("/handle/0/routes")
            .unwrap()
            .as_array()
            .unwrap()
            .len(),
        1
    );
    fs::remove_dir_all(directory).unwrap();
}

#[test]
fn ordinary_list_reconciles_reload_and_records_synchronization() {
    let (directory, config_home, state_home) = temporary_homes("automatic-reconcile");
    let routes = Arc::new(Mutex::new(Vec::<Value>::new()));
    let first_listener = TcpListener::bind("127.0.0.1:0").unwrap();
    write_global_config(&config_home, first_listener.local_addr().unwrap());
    let first_routes = Arc::clone(&routes);
    let first_server = thread::spawn(move || serve_caddy(first_listener, first_routes, 4));
    assert_success(&nook(&config_home, &state_home, &["alias", "api", "3000"]));
    first_server.join().unwrap();
    routes.lock().unwrap().clear();

    let second_listener = TcpListener::bind("127.0.0.1:0").unwrap();
    write_global_config(&config_home, second_listener.local_addr().unwrap());
    let second_routes = Arc::clone(&routes);
    let second_server = thread::spawn(move || serve_caddy(second_listener, second_routes, 3));
    let list = nook(&config_home, &state_home, &["list"]);
    assert_success(&list);
    assert!(
        String::from_utf8(list.stdout)
            .unwrap()
            .contains("api.localhost")
    );
    second_server.join().unwrap();
    assert!(!routes.lock().unwrap().is_empty());
    let state: Value =
        serde_json::from_slice(&fs::read(state_home.join("nook/state.json")).unwrap()).unwrap();
    assert_eq!(state["selected_servers"]["https"], "https");
    assert!(state["last_synchronized_at_unix_ms"].as_u64().is_some());
    fs::remove_dir_all(directory).unwrap();
}

#[test]
fn force_refuses_a_foreign_caddy_route_without_mutating_it() {
    let (directory, config_home, state_home) = temporary_homes("foreign");
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    write_global_config(&config_home, listener.local_addr().unwrap());
    let foreign = json!({
        "match": [{"host": ["foreign.localhost"]}],
        "handle": [{"handler": "static_response", "body": "foreign"}]
    });
    let routes = Arc::new(Mutex::new(vec![foreign.clone()]));
    let server_routes = Arc::clone(&routes);
    let server = thread::spawn(move || serve_caddy(listener, server_routes, 3));
    let set = nook(
        &config_home,
        &state_home,
        &["alias", "set", "foreign", "3000", "--force"],
    );
    assert_eq!(set.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&set.stderr).contains("foreign Caddy route"));
    server.join().unwrap();
    assert_eq!(*routes.lock().unwrap(), [foreign]);
    fs::remove_dir_all(directory).unwrap();
}

#[test]
fn concurrent_clis_create_and_remove_aliases_without_lost_updates() {
    let (directory, config_home, state_home) = temporary_homes("concurrent");
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    write_global_config(&config_home, listener.local_addr().unwrap());
    let routes = Arc::new(Mutex::new(Vec::<Value>::new()));
    let server_routes = Arc::clone(&routes);
    let server = thread::spawn(move || serve_caddy(listener, server_routes, 29));
    let create: Vec<_> = [("one", "3001"), ("two", "3002")]
        .into_iter()
        .map(|(name, port)| {
            let config_home = config_home.clone();
            let state_home = state_home.clone();
            thread::spawn(move || nook(&config_home, &state_home, &["alias", name, port]))
        })
        .collect();
    for output in create {
        assert_success(&output.join().unwrap());
    }
    let list = nook(&config_home, &state_home, &["alias", "list"]);
    assert_success(&list);
    let list = String::from_utf8(list.stdout).unwrap();
    assert!(list.contains("one.localhost"));
    assert!(list.contains("two.localhost"));

    let remove: Vec<_> = ["one", "two"]
        .into_iter()
        .map(|name| {
            let config_home = config_home.clone();
            let state_home = state_home.clone();
            thread::spawn(move || nook(&config_home, &state_home, &["alias", "remove", name]))
        })
        .collect();
    for output in remove {
        assert_success(&output.join().unwrap());
    }
    server.join().unwrap();
    assert!(routes.lock().unwrap().is_empty());
    assert!(
        nook(&config_home, &state_home, &["alias", "list"])
            .stdout
            .is_empty()
    );
    fs::remove_dir_all(directory).unwrap();
}

fn temporary_homes(name: &str) -> (std::path::PathBuf, std::path::PathBuf, std::path::PathBuf) {
    let directory = std::env::temp_dir().join(format!(
        "nook-cli-{name}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let config_home = directory.join("config");
    let state_home = directory.join("state");
    fs::create_dir_all(config_home.join("nook")).unwrap();
    (directory, config_home, state_home)
}

fn write_global_config(config_home: &Path, address: std::net::SocketAddr) {
    fs::write(
        config_home.join("nook/config.toml"),
        format!("format_version = 1\ncaddy_admin = \"http://{address}\"\n"),
    )
    .unwrap();
}

fn wait_for_lease(state_home: &Path, hostname: &str) {
    wait_for_lease_state(state_home, hostname, "ready");
}

fn wait_for_lease_state(state_home: &Path, hostname: &str, state_name: &str) {
    let state_path = state_home.join("nook/state.json");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    while std::time::Instant::now() < deadline {
        if fs::read_to_string(&state_path).is_ok_and(|state| {
            state.contains(hostname) && state.contains(&format!("\"state\": \"{state_name}\""))
        }) {
            return;
        }
        thread::sleep(std::time::Duration::from_millis(20));
    }
    panic!("lease {hostname} did not become {state_name}");
}

fn nook(config_home: &Path, state_home: &Path, arguments: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_nook"))
        .env("NOOK_DISABLE_UPDATE_CHECK", "1")
        .args(arguments)
        .env("XDG_CONFIG_HOME", config_home)
        .env("XDG_STATE_HOME", state_home)
        .output()
        .unwrap()
}

fn assert_success(output: &Output) {
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn serve_caddy(listener: TcpListener, routes: Arc<Mutex<Vec<Value>>>, requests: usize) {
    for _ in 0..requests {
        let (mut stream, _) = listener.accept().unwrap();
        let request = read_request(&mut stream);
        let header_end = request
            .windows(4)
            .position(|bytes| bytes == b"\r\n\r\n")
            .unwrap()
            + 4;
        let head = String::from_utf8_lossy(&request[..header_end]);
        let first_line = head.lines().next().unwrap();
        if first_line.starts_with("GET /config/ ") {
            let current = routes.lock().unwrap().clone();
            respond_json(
                &mut stream,
                &json!({"apps":{"http":{"servers":{"https":{"listen":[":443"],"routes":current}}}}}),
                None,
            );
        } else if first_line.starts_with("GET /pki/ca/local ") {
            respond_bytes(
                &mut stream,
                b"-----BEGIN CERTIFICATE-----\nMAMCAQE=\n-----END CERTIFICATE-----\n",
                "application/pem-certificate-chain",
                None,
            );
        } else if first_line.starts_with("GET /config/apps/http/servers/https/routes ") {
            respond_json(&mut stream, &json!(*routes.lock().unwrap()), Some("\"v1\""));
        } else if first_line.starts_with("PATCH /config/apps/http/servers/https/routes ") {
            *routes.lock().unwrap() = serde_json::from_slice(&request[header_end..]).unwrap();
            respond_json(&mut stream, &json!({}), None);
        } else {
            panic!("unexpected request: {first_line}");
        }
    }
}

fn read_request(stream: &mut TcpStream) -> Vec<u8> {
    let mut request = Vec::new();
    let mut buffer = [0_u8; 4096];
    let mut expected = None;
    loop {
        let read = stream.read(&mut buffer).unwrap();
        assert!(read > 0);
        request.extend_from_slice(&buffer[..read]);
        if expected.is_none()
            && let Some(header_end) = request.windows(4).position(|bytes| bytes == b"\r\n\r\n")
        {
            let head = String::from_utf8_lossy(&request[..header_end]);
            let length = head
                .lines()
                .filter_map(|line| line.split_once(':'))
                .find_map(|(name, value)| {
                    name.eq_ignore_ascii_case("content-length")
                        .then(|| value.trim())
                })
                .and_then(|value| value.parse::<usize>().ok())
                .unwrap_or(0);
            expected = Some(header_end + 4 + length);
        }
        if expected.is_some_and(|length| request.len() >= length) {
            return request;
        }
    }
}

fn respond_json(stream: &mut TcpStream, value: &Value, etag: Option<&str>) {
    let body = serde_json::to_vec(value).unwrap();
    respond_bytes(stream, &body, "application/json", etag);
}

fn respond_bytes(stream: &mut TcpStream, body: &[u8], content_type: &str, etag: Option<&str>) {
    let etag = etag.map_or(String::new(), |value| format!("ETag: {value}\r\n"));
    write!(
        stream,
        "HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\n{etag}Content-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    )
    .unwrap();
    stream.write_all(body).unwrap();
}
