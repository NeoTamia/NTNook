//! Real-binary workspace tests. Unix hosts need Python 3 and Caddy 2.11.x;
//! Linux uses an isolated network namespace unless NOOK_E2E_DIRECT is set,
//! matching nook_caddy_e2e. Windows needs a native child fixture before enabling.
#![cfg(unix)]

mod support;

use std::fs::{self, File};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use support::caddy::CaddyHarness;

const CHILD: &str = r#"
import os, pathlib
pathlib.Path('spawned').write_text(str(os.getpid()))
import http.client, http.server, json, signal, socket, sys, time
mode = os.environ.get('SHUTDOWN_HANDLER')
if mode:
    def stop(sig, frame):
        def repeated(sig, frame):
            pathlib.Path('duplicate').write_text(str(sig))
        signal.signal(signal.SIGINT, repeated)
        if mode == 'both':
            signal.signal(signal.SIGTERM, repeated)
        pathlib.Path('stopping').write_text(str(sig))
        time.sleep(.35)
        pathlib.Path('clean').write_text(str(sig))
        sys.exit(0)
    signal.signal(signal.SIGINT, signal.SIG_IGN if mode == 'stubborn' else stop)
    if mode in ['both', 'stubborn']:
        signal.signal(signal.SIGTERM, signal.SIG_IGN if mode == 'stubborn' else stop)
if os.environ.get('SPAWN_DESCENDANT'):
    import subprocess
    descendant = subprocess.Popen([sys.executable, '-c', 'import time; time.sleep(600)'])
    pathlib.Path('descendant').write_text(str(descendant.pid))
# Capture Caddy before opening our listener: both sibling routes must exist now.
admin = http.client.HTTPConnection('localhost', timeout=3)
admin.sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
admin.sock.settimeout(3)
admin.sock.connect(os.environ['TEST_CADDY_SOCKET'])
admin.request('GET', '/config/')
routes = json.load(admin.getresponse())
admin.close()
server = http.server.HTTPServer((os.environ['HOST'], int(os.environ['PORT'])), http.server.SimpleHTTPRequestHandler)
report = dict(pid=os.getpid(), cwd=os.getcwd(), argv=sys.argv[1:], env=dict(os.environ), routes=routes)
pathlib.Path('started.tmp').write_text(json.dumps(report))
pathlib.Path('started.tmp').rename('started.json')
print('child-stdout', flush=True)
print('child-stderr', file=sys.stderr, flush=True)
if os.environ.get('FAIL_AFTER_SIBLING') or os.environ.get('EXIT_AFTER_SIBLING'):
    deadline = time.monotonic() + 5
    while not pathlib.Path('../api/started.json').exists():
        assert time.monotonic() < deadline, 'sibling never started'
        time.sleep(.01)
    sys.exit(int(os.environ.get('EXIT_AFTER_SIBLING', '23')))
server.serve_forever()
"#;

#[test]
fn workspace_init_writes_a_v2_skeleton_without_overwriting() {
    let root = std::env::temp_dir().join(format!("nook-workspace-init-{}", uuid::Uuid::new_v4()));
    fs::create_dir(&root).unwrap();
    let init = || {
        Command::new(env!("CARGO_BIN_EXE_nook"))
            .current_dir(&root)
            .env("XDG_CONFIG_HOME", root.join("config"))
            .env("XDG_STATE_HOME", root.join("state"))
            .env("NOOK_DISABLE_UPDATE_CHECK", "1")
            .args(["init", "--workspace", "--name", "project"])
            .output()
            .unwrap()
    };
    let output = init();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let original = fs::read_to_string(root.join("nook.toml")).unwrap();
    let config: toml::Value = toml::from_str(&original).unwrap();
    assert_eq!(config["format_version"].as_integer(), Some(2));
    assert_eq!(config["name"].as_str(), Some("project"));
    let apps = config["apps"].as_array().unwrap();
    assert!(!apps.is_empty());
    for app in apps {
        assert!(!app["id"].as_str().unwrap().is_empty());
        assert!(app["dir"].as_str().is_some());
        assert!(!app["command"].as_array().unwrap().is_empty());
    }
    assert!(!init().status.success());
    assert_eq!(
        fs::read_to_string(root.join("nook.toml")).unwrap(),
        original
    );
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn workspace_prepares_siblings_before_spawn_and_cleans_up_on_ctrl_c() {
    if in_network_namespace("workspace_prepares_siblings_before_spawn_and_cleans_up_on_ctrl_c") {
        return;
    }
    let fixture = Workspace::new();
    fixture.configure(true, None, "");
    let mut running = fixture.start(&["run"]);
    let reports = fixture.wait_ready(&mut running, &["api", "web"]);
    let ports: Vec<_> = reports
        .iter()
        .map(|report| report["env"]["PORT"].as_str().unwrap())
        .collect();
    assert_ne!(ports[0], ports[1]);
    for (index, id) in ["api", "web"].iter().enumerate() {
        let peer = 1 - index;
        let report = &reports[index];
        assert_eq!(
            report["cwd"],
            fixture.root.join(format!("apps/{id}")).to_str().unwrap()
        );
        assert_eq!(report["argv"], json!(["literal with spaces", "$PORT", ";"]));
        assert_eq!(report["env"]["APP_ID"], *id);
        // A forward reference must work even though api is first in the manifest.
        if *id == "api" {
            assert_eq!(report["env"]["PEER_PORT"], ports[peer]);
            assert_eq!(report["env"]["PEER_HOST"], reports[peer]["env"]["HOST"]);
            assert_eq!(
                report["env"]["PEER_URL"],
                format!("http://{}", fixture.host("web"))
            );
            assert_eq!(
                report["env"]["PEER_UPSTREAM"]
                    .as_str()
                    .unwrap()
                    .trim_end_matches('/'),
                format!("http://127.0.0.1:{}", ports[peer])
            );
        }
        let routes = report["routes"].to_string();
        for (sibling, port) in ["api", "web"].iter().zip(&ports) {
            assert!(
                routes.contains(&fixture.host(sibling)),
                "{id} spawned before {sibling}'s route existed"
            );
            assert!(
                routes.contains(&format!("127.0.0.1:{port}")),
                "{id} saw an unprepared upstream"
            );
        }
        assert!(fixture.http(id).contains("child.py"));
    }
    let state = fixture.state();
    let leases: Vec<_> = state["leases"].as_object().unwrap().values().collect();
    assert_eq!(leases.len(), 2);
    assert!(
        leases[0]["group_id"].as_str().is_some(),
        "workspace leases need a group_id"
    );
    assert_eq!(leases[0]["group_id"], leases[1]["group_id"]);
    assert_ne!(leases[0]["id"], leases[1]["id"]);
    for lease in leases {
        assert!(!lease["tls"].as_bool().unwrap());
        assert!(reports.iter().any(|report| report["pid"] == lease["pid"]));
    }
    // Reconciliation must preserve both live members.
    assert!(fixture.run(&["prune"]).success());
    assert_eq!(fixture.state()["leases"].as_object().unwrap().len(), 2);
    running.signal(libc::SIGINT);
    assert_eq!(running.wait().code(), Some(130), "{}", running.logs());
    for id in ["api", "web"] {
        assert!(
            running.stdout().contains(&format!("[{id}] child-stdout")),
            "{}",
            running.logs()
        );
        assert!(
            running.stderr().contains(&format!("[{id}] child-stderr")),
            "{}",
            running.logs()
        );
    }
    fixture.assert_clean(&reports);
}

#[test]
fn workspace_ctrl_c_allows_async_cleanup_with_default_sigterm() {
    if in_network_namespace("workspace_ctrl_c_allows_async_cleanup_with_default_sigterm") {
        return;
    }
    let fixture = Workspace::new();
    fixture.configure(false, None, "SHUTDOWN_HANDLER = 'interrupt'\n");
    let mut running = fixture.start(&["run", "--only", "web"]);
    let reports = fixture.wait_ready(&mut running, &["web"]);
    running.signal(libc::SIGINT);
    assert_eq!(running.wait().code(), Some(130), "{}", running.logs());
    assert_eq!(
        fs::read_to_string(fixture.root.join("apps/web/clean")).ok(),
        Some(libc::SIGINT.to_string()),
        "SIGTERM interrupted the async Ctrl-C handler: {}",
        running.logs()
    );
    fixture.assert_clean(&reports);
}

#[test]
fn workspace_shutdown_delivers_the_chosen_signal_once() {
    if in_network_namespace("workspace_shutdown_delivers_the_chosen_signal_once") {
        return;
    }
    for (signal, code) in [(libc::SIGINT, 130), (libc::SIGTERM, 143)] {
        let fixture = Workspace::new();
        fixture.configure(false, None, "SHUTDOWN_HANDLER = 'both'\n");
        let mut running = fixture.start(&["run", "--only", "web"]);
        let reports = fixture.wait_ready(&mut running, &["web"]);
        running.signal(signal);
        assert_eq!(running.wait().code(), Some(code), "{}", running.logs());
        assert_eq!(
            fs::read_to_string(fixture.root.join("apps/web/clean")).ok(),
            Some(signal.to_string()),
            "{}",
            running.logs()
        );
        assert!(
            !fixture.root.join("apps/web/duplicate").exists(),
            "shutdown delivered a second signal: {}",
            running.logs()
        );
        fixture.assert_clean(&reports);
    }
}

#[test]
fn workspace_shell_leader_exit_preserves_descendant_cleanup() {
    if in_network_namespace("workspace_shell_leader_exit_preserves_descendant_cleanup") {
        return;
    }
    for (signal, code) in [(libc::SIGINT, 130), (libc::SIGTERM, 143)] {
        let fixture = Workspace::new();
        let command = json!([
            "/bin/sh",
            "-c",
            "trap 'exit 130' INT; trap 'exit 143' TERM; python3 -u child.py & wait $!"
        ])
        .to_string();
        fixture.configure(false, Some(&command), "SHUTDOWN_HANDLER = 'both'\n");
        let mut running = fixture.start(&["run", "--only", "web"]);
        let reports = fixture.wait_ready(&mut running, &["web"]);
        running.signal(signal);
        let deadline = Instant::now() + Duration::from_secs(3);
        while !fixture.root.join("apps/web/stopping").exists() {
            assert!(Instant::now() < deadline, "{}", running.logs());
            thread::sleep(Duration::from_millis(5));
        }
        // A further supervisor signal must not truncate the descendant's grace.
        running.signal(signal);
        assert_eq!(running.wait().code(), Some(code), "{}", running.logs());
        assert_eq!(
            fs::read_to_string(fixture.root.join("apps/web/clean")).ok(),
            Some(signal.to_string()),
            "shell exit bypassed descendant cleanup: {}",
            running.logs()
        );
        assert!(!fixture.root.join("apps/web/duplicate").exists());
        fixture.assert_clean(&reports);
    }
}

#[test]
fn workspace_shutdown_kills_stubborn_children_after_bounded_grace() {
    if in_network_namespace("workspace_shutdown_kills_stubborn_children_after_bounded_grace") {
        return;
    }
    let fixture = Workspace::new();
    fixture.configure(false, None, "SHUTDOWN_HANDLER = 'stubborn'\n");
    let mut running = fixture.start(&["run", "--only", "web"]);
    let reports = fixture.wait_ready(&mut running, &["web"]);
    let started = Instant::now();
    running.signal(libc::SIGINT);
    assert_eq!(running.wait().code(), Some(130), "{}", running.logs());
    assert!(started.elapsed() >= Duration::from_secs(2));
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "{}",
        running.logs()
    );
    fixture.assert_clean(&reports);
}

#[test]
#[cfg(target_os = "linux")]
fn workspace_detached_descendants_receive_graceful_shutdown() {
    if in_network_namespace("workspace_detached_descendants_receive_graceful_shutdown") {
        return;
    }
    for signal in [libc::SIGINT, libc::SIGTERM] {
        native_descendant_shutdown(true, signal);
    }
}

#[test]
#[cfg(target_os = "linux")]
fn workspace_reaped_leader_descendants_receive_graceful_shutdown() {
    if in_network_namespace("workspace_reaped_leader_descendants_receive_graceful_shutdown") {
        return;
    }
    native_descendant_shutdown(false, libc::SIGTERM);
}

#[cfg(target_os = "linux")]
fn native_descendant_shutdown(detached: bool, signal: i32) {
    let fixture = Workspace::new();
    fs::write(
        fixture.root.join("apps/web/launcher.py"),
        format!(
            "import pathlib,subprocess,sys,time\nchild=subprocess.Popen([sys.executable,'-u','child.py'],start_new_session={})\nwhile not pathlib.Path('exit-leader').exists(): time.sleep(.01)\nsys.exit(23)\n",
            if detached { "True" } else { "False" }
        ),
    )
    .unwrap();
    let command = json!(["python3", "-u", "launcher.py"]).to_string();
    fixture.configure(false, Some(&command), "SHUTDOWN_HANDLER = 'both'\n");
    let mut running = fixture.start(&["run", "--only", "web"]);
    let reports = fixture.wait_ready(&mut running, &["web"]);
    let started = Instant::now();
    if detached {
        running.signal(signal);
    } else {
        // finish() reaps the leader before selecting SIGTERM for its tree.
        fs::write(fixture.root.join("apps/web/exit-leader"), "").unwrap();
    }
    assert_eq!(
        running.wait().code(),
        Some(if detached { 128 + signal } else { 23 }),
        "{}",
        running.logs()
    );
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "{}",
        running.logs()
    );
    assert_eq!(
        fs::read_to_string(fixture.root.join("apps/web/clean")).ok(),
        Some(signal.to_string()),
        "descendant missed graceful shutdown: {}",
        running.logs()
    );
    assert!(
        !fixture.root.join("apps/web/duplicate").exists(),
        "{}",
        running.logs()
    );
    fixture.assert_clean(&reports);
}

#[test]
fn workspace_member_exit_sends_term_and_preserves_the_exit_code() {
    if in_network_namespace("workspace_member_exit_sends_term_and_preserves_the_exit_code") {
        return;
    }
    for code in [0, 23] {
        let fixture = Workspace::new();
        fixture.configure(false, None, &format!("EXIT_AFTER_SIBLING = '{code}'\n"));
        let mut command = fixture.nook();
        command.arg("run").env("SHUTDOWN_HANDLER", "both");
        let mut running = Running::start(command, &fixture.root, "run");
        assert_eq!(running.wait().code(), Some(code), "{}", running.logs());
        assert_eq!(
            fs::read_to_string(fixture.root.join("apps/api/clean")).ok(),
            Some(libc::SIGTERM.to_string()),
            "{}",
            running.logs()
        );
        assert!(!fixture.root.join("apps/api/duplicate").exists());
        fixture.assert_clean(&[
            fixture.report("api").unwrap(),
            fixture.report("web").unwrap(),
        ]);
    }
}

#[test]
fn workspace_only_and_standalone_member_keep_their_own_scope() {
    if in_network_namespace("workspace_only_and_standalone_member_keep_their_own_scope") {
        return;
    }
    let fixture = Workspace::new();
    fixture.configure(false, None, "");
    let port = TcpListener::bind(("127.0.0.1", 0))
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let path = fixture.root.join("nook.toml");
    let mut config: toml::Value = toml::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
    config["apps"][0]
        .as_table_mut()
        .unwrap()
        .insert("app_port".into(), toml::Value::Integer(port.into()));
    fs::write(path, toml::to_string(&config).unwrap()).unwrap();
    let mut running = fixture.start(&["run", "--only", "api"]);
    let reports = fixture.wait_ready(&mut running, &["api"]);
    assert_eq!(reports[0]["env"]["PORT"], port.to_string());
    assert!(!fixture.root.join("apps/web/spawned").exists());
    assert_eq!(fixture.state()["leases"].as_object().unwrap().len(), 1);
    assert!(!fixture.caddy.config_json().contains(&fixture.host("web")));
    running.signal(libc::SIGINT);
    running.wait();
    fixture.assert_clean(&reports);

    fs::remove_file(fixture.report_path("api")).unwrap();
    let name = fixture.host("api");
    fs::write(
        fixture.root.join("apps/api/nook.toml"),
        format!(
            "format_version = 1\nname = {name:?}\ntls = false\ncommand = {}\n",
            fixture.command()
        ),
    )
    .unwrap();
    let mut command = fixture.nook();
    command
        .current_dir(fixture.root.join("apps/api"))
        .arg("run");
    let mut standalone = Running::start(command, &fixture.root, "standalone");
    let reports = fixture.wait_ready(&mut standalone, &["api"]);
    assert!(!fixture.root.join("apps/web/spawned").exists());
    assert_eq!(fixture.state()["leases"].as_object().unwrap().len(), 1);
    standalone.signal(libc::SIGINT);
    assert_eq!(standalone.wait().code(), Some(130), "{}", standalone.logs());
    fixture.assert_clean(&reports);
}

#[test]
fn workspace_invalid_config_or_selection_never_spawns_a_member() {
    if in_network_namespace("workspace_invalid_config_or_selection_never_spawns_a_member") {
        return;
    }
    let fixture = Workspace::new();
    let original = fixture.caddy.config_json();
    for (web_command, extra, arguments) in [
        (None, "BAD = '${missing.upstream}'\n", vec!["run"]),
        (Some("[]"), "", vec!["run"]),
        (None, "", vec!["run", "--only", "missing"]),
    ] {
        fixture.configure(false, web_command, extra);
        let mut running = fixture.start(&arguments);
        assert!(!running.wait().success(), "{}", running.logs());
        for id in ["api", "web"] {
            assert!(
                !fixture.root.join(format!("apps/{id}/spawned")).exists(),
                "{id} spawned before validation finished"
            );
        }
        assert_eq!(
            fixture.caddy.config_json(),
            original,
            "invalid input changed Caddy routes"
        );
        fixture.assert_clean(&[]);
    }
}

#[test]
fn workspace_child_exit_and_spawn_failure_roll_back_the_group() {
    if in_network_namespace("workspace_child_exit_and_spawn_failure_roll_back_the_group") {
        return;
    }
    let fixture = Workspace::new();
    fixture.configure(false, None, "FAIL_AFTER_SIBLING = '1'\n");
    let mut running = fixture.start(&["run"]);
    assert!(!running.wait().success(), "{}", running.logs());
    let reports = [
        fixture.report("api").unwrap(),
        fixture.report("web").unwrap(),
    ];
    fixture.assert_clean(&reports);
    for id in ["api", "web"] {
        fs::remove_file(fixture.report_path(id)).unwrap();
        fs::remove_file(fixture.root.join(format!("apps/{id}/spawned"))).unwrap();
    }

    fixture.configure(false, Some("['/does/not/exist/nook-workspace-child']"), "");
    let mut failed = fixture.start(&["run"]);
    assert!(!failed.wait().success(), "{}", failed.logs());
    let started: Vec<_> = ["api", "web"]
        .iter()
        .filter_map(|id| fixture.report(id))
        .collect();
    fixture.assert_clean(&started);
}

#[test]
fn workspace_supervisor_sigkill_then_prune_leaves_no_children_or_routes() {
    if in_network_namespace("workspace_supervisor_sigkill_then_prune_leaves_no_children_or_routes")
    {
        return;
    }
    let fixture = Workspace::new();
    fixture.configure(true, None, "SPAWN_DESCENDANT = '1'\n");
    let mut running = fixture.start(&["run"]);
    let reports = fixture.wait_ready(&mut running, &["api", "web"]);
    running.signal(libc::SIGKILL);
    assert!(!running.wait().success());
    assert!(fixture.run(&["prune"]).success());
    fixture.assert_clean(&reports);
}

#[test]
fn linked_worktrees_keep_native_and_runner_apps_isolated_through_caddy_and_lan() {
    if in_network_namespace(
        "linked_worktrees_keep_native_and_runner_apps_isolated_through_caddy_and_lan",
    ) {
        return;
    }
    for runner in [false, true] {
        let fixture = Workspace::new();
        fixture.configure(true, None, "");
        let manifest = fixture.root.join("nook.toml");
        let original = fs::read_to_string(&manifest).unwrap().replace(
            "PEER_URL = '${web.url}'",
            "PEER_LAN = '${web.lan_url}'\nPEER_URL = '${web.url}'",
        );
        fs::write(
            &manifest,
            original.replacen(
                "format_version = 2",
                "format_version = 2\nworktree = true",
                1,
            ),
        )
        .unwrap();
        fs::write(fixture.root.join("runner.py"), r#"
import os, subprocess, time
children = [subprocess.Popen([os.environ['TEST_NOOK'], 'run', '--', 'python3', '-u', 'child.py'], cwd='apps/' + app) for app in ['api', 'web']]
while all(child.poll() is None for child in children): time.sleep(.02)
"#).unwrap();
        let git = |args: &[&str]| {
            let output = Command::new("git")
                .current_dir(&fixture.root)
                .args(args)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
        };
        git(&["init"]);
        git(&["add", "nook.toml", "apps", "runner.py"]);
        git(&[
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.invalid",
            "commit",
            "-m",
            "fixture",
        ]);
        let linked = fixture.caddy.root().join("agent-b");
        git(&["worktree", "add", "--detach", linked.to_str().unwrap()]);
        let start = |root: &std::path::Path, label: &str| {
            let mut command = fixture.nook();
            command
                .current_dir(root)
                .env("TEST_NOOK", env!("CARGO_BIN_EXE_nook"))
                .args(["run", "--lan", &format!("--worktree={label}")]);
            if runner {
                command.args(["--runner", "--", "python3", "runner.py"]);
            }
            Running::start(command, &fixture.root, label)
        };
        // Start concurrently, sharing the same registry and Caddy instance.
        let mut first = start(&fixture.root, "agent-a");
        let mut second = start(&linked, "agent-b");
        let deadline = Instant::now() + Duration::from_secs(12);
        let reports = loop {
            let reports: Vec<Value> = [&fixture.root, &linked]
                .into_iter()
                .flat_map(|root| {
                    ["api", "web"].map(|app| root.join(format!("apps/{app}/started.json")))
                })
                .filter_map(|path| {
                    fs::read(path)
                        .ok()
                        .map(|bytes| serde_json::from_slice(&bytes).unwrap())
                })
                .collect();
            let state = fixture.state();
            let leases = state["leases"].as_object().unwrap();
            if reports.len() == 4
                && leases.len() == 4
                && leases.values().all(|lease| lease["state"] == "ready")
            {
                break reports;
            }
            assert!(
                first.child.try_wait().unwrap().is_none(),
                "{}",
                first.logs()
            );
            assert!(
                second.child.try_wait().unwrap().is_none(),
                "{}",
                second.logs()
            );
            assert!(
                Instant::now() < deadline,
                "{}\n{}",
                first.logs(),
                second.logs()
            );
            thread::sleep(Duration::from_millis(20));
        };
        let ports: std::collections::BTreeSet<_> = reports
            .iter()
            .map(|r| r["env"]["PORT"].as_str().unwrap())
            .collect();
        assert_eq!(ports.len(), 4);
        let get = |host: &str| {
            let mut stream = TcpStream::connect(("127.0.0.1", 80)).unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            write!(
                stream,
                "GET /started.json HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n"
            )
            .unwrap();
            let mut response = String::new();
            stream.read_to_string(&mut response).unwrap();
            assert!(response.starts_with("HTTP/1.1 200"), "{response}");
            response
        };
        for (offset, label) in [(0, "agent-a"), (2, "agent-b")] {
            assert_eq!(
                reports[offset]["env"]["PEER_URL"],
                format!("http://frontend.{label}.project.localhost")
            );
            assert_eq!(
                reports[offset]["env"]["PEER_LAN"],
                format!("http://frontend.{label}.project.local")
            );
            assert_eq!(
                reports[offset]["env"]["PEER_PORT"],
                reports[offset + 1]["env"]["PORT"]
            );
            for (index, app) in ["api", "frontend"].iter().enumerate() {
                let host = format!("{app}.{label}.project");
                assert_eq!(
                    reports[offset + index]["env"]["NOOK_LAN_URL"],
                    format!("http://{host}.local")
                );
                for suffix in ["localhost", "local"] {
                    let response = get(&format!("{host}.{suffix}"));
                    assert!(response.contains(reports[offset + index]["cwd"].as_str().unwrap()));
                }
            }
        }
        let before = fixture.state();
        let mut collision = fixture.nook();
        collision
            .current_dir(&linked)
            .args(["run", "--worktree=agent-a", "--force", "--lan"]);
        let output = collision.output().unwrap();
        assert!(!output.status.success());
        assert!(String::from_utf8_lossy(&output.stderr).contains("another worktree"));
        assert_eq!(fixture.state()["leases"], before["leases"]);
        // A killed supervisor is recovered without removing the other worktree.
        first.signal(libc::SIGKILL);
        first.wait();
        assert!(fixture.run(&["prune"]).success());
        assert_eq!(fixture.state()["leases"].as_object().unwrap().len(), 2);
        assert!(second.child.try_wait().unwrap().is_none());
        for suffix in ["localhost", "local"] {
            get(&format!("frontend.agent-b.project.{suffix}"));
        }
        assert!(!fixture.caddy.config_json().contains("agent-a.project"));
        second.signal(libc::SIGINT);
        second.wait();
        fixture.assert_clean(&reports);
        assert!(!fixture.caddy.config_json().contains("agent-b.project"));
    }
}

struct Workspace {
    root: PathBuf,
    caddy: CaddyHarness,
}

impl Workspace {
    fn new() -> Self {
        let caddy = CaddyHarness::start_on_standard_ports();
        let root = caddy.root().join("workspace");
        fs::create_dir_all(root.join("config/nook")).unwrap();
        for id in ["api", "web"] {
            fs::create_dir_all(root.join(format!("apps/{id}"))).unwrap();
            fs::write(root.join(format!("apps/{id}/child.py")), CHILD).unwrap();
        }
        fs::write(
            root.join("config/nook/config.toml"),
            format!(
                "format_version = 1\ncaddy_admin = {:?}\n",
                caddy.admin_url()
            ),
        )
        .unwrap();
        Self { root, caddy }
    }

    fn host(&self, id: &str) -> String {
        format!(
            "{}.project.localhost",
            if id == "web" { "frontend" } else { id }
        )
    }

    fn command(&self) -> String {
        json!([
            "python3",
            "-u",
            "child.py",
            "literal with spaces",
            "$PORT",
            ";"
        ])
        .to_string()
    }

    fn configure(&self, references: bool, web_command: Option<&str>, extra: &str) {
        let mut config = "format_version = 2\nname = 'project'\n".to_string();
        for (id, peer) in [("api", "web"), ("web", "api")] {
            config.push_str(&format!(
                "\n[[apps]]\nid = '{id}'\ndir = 'apps/{id}'\ntls = false\ncommand = {}\n",
                if id == "web" {
                    web_command
                        .map(str::to_owned)
                        .unwrap_or_else(|| self.command())
                } else {
                    self.command()
                }
            ));
            if id == "web" {
                config.push_str("name = 'frontend.project'\n");
            }
            config.push_str(&format!("[apps.env]\nAPP_ID = '{id}'\n"));
            if references && id == "api" {
                config.push_str(&format!(
                    "PEER_URL = '${{{peer}.url}}'\nPEER_HOST = '${{{peer}.host}}'\nPEER_PORT = '${{{peer}.port}}'\nPEER_UPSTREAM = '${{{peer}.upstream}}'\n"
                ));
            }
            if id == "web" {
                config.push_str(extra);
            }
        }
        fs::write(self.root.join("nook.toml"), config).unwrap();
    }

    fn nook(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_nook"));
        command
            .current_dir(&self.root)
            .env("XDG_CONFIG_HOME", self.root.join("config"))
            .env("XDG_STATE_HOME", self.root.join("state"))
            .env("NOOK_DISABLE_UPDATE_CHECK", "1")
            .env("TEST_CADDY_SOCKET", self.caddy.admin_socket().unwrap());
        command
    }

    fn start(&self, arguments: &[&str]) -> Running {
        let mut command = self.nook();
        command.args(arguments);
        Running::start(command, &self.root, "run")
    }

    fn run(&self, arguments: &[&str]) -> ExitStatus {
        let mut running = self.start(arguments);
        let status = running.wait();
        assert!(status.success(), "{}", running.logs());
        status
    }

    fn state(&self) -> Value {
        let path = self.root.join("state/nook/state.json");
        match fs::read(path) {
            Ok(bytes) => serde_json::from_slice(&bytes).unwrap(),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                json!({"leases": {}, "pending_operations": []})
            }
            Err(error) => panic!("cannot read workspace state: {error}"),
        }
    }

    fn report_path(&self, id: &str) -> PathBuf {
        self.root.join(format!("apps/{id}/started.json"))
    }

    fn report(&self, id: &str) -> Option<Value> {
        fs::read(self.report_path(id))
            .ok()
            .map(|bytes| serde_json::from_slice(&bytes).unwrap())
    }

    fn wait_ready(&self, running: &mut Running, ids: &[&str]) -> Vec<Value> {
        let deadline = Instant::now() + Duration::from_secs(8);
        loop {
            let reports: Vec<_> = ids.iter().filter_map(|id| self.report(id)).collect();
            let state = self.state();
            if reports.len() == ids.len()
                && state["leases"].as_object().unwrap().len() == ids.len()
                && state["leases"]
                    .as_object()
                    .unwrap()
                    .values()
                    .all(|lease| lease["state"] == "ready")
            {
                return reports;
            }
            assert!(
                running.child.try_wait().unwrap().is_none(),
                "supervisor exited before readiness: {}",
                running.logs()
            );
            assert!(
                Instant::now() < deadline,
                "workspace never became ready: {}",
                running.logs()
            );
            thread::sleep(Duration::from_millis(20));
        }
    }

    fn http(&self, id: &str) -> String {
        let mut stream = TcpStream::connect(("127.0.0.1", 80)).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        write!(
            stream,
            "GET / HTTP/1.1\r\nHost: {}\r\nConnection: close\r\n\r\n",
            self.host(id)
        )
        .unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).unwrap();
        assert!(response.starts_with("HTTP/1.1 200"), "{response}");
        response
    }

    fn assert_clean(&self, reports: &[Value]) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while self.spawned_pids().into_iter().any(alive) {
            assert!(Instant::now() < deadline, "workspace left a child alive");
            thread::sleep(Duration::from_millis(20));
        }
        let state = self.state();
        assert!(state["leases"].as_object().unwrap().is_empty(), "{state}");
        assert!(
            state
                .get("workspace_groups")
                .is_none_or(|groups| groups.as_object().unwrap().is_empty()),
            "{state}"
        );
        assert!(
            state["pending_operations"].as_array().unwrap().is_empty(),
            "{state}"
        );
        let config = self.caddy.config_json();
        for id in ["api", "web"] {
            assert!(
                !config.contains(&self.host(id)),
                "route for {id} survived cleanup"
            );
        }
        for report in reports {
            let port: u16 = report["env"]["PORT"].as_str().unwrap().parse().unwrap();
            assert!(
                TcpStream::connect(("127.0.0.1", port)).is_err(),
                "port {port} still has a listener"
            );
        }
    }

    fn spawned_pids(&self) -> Vec<i32> {
        ["api", "web"]
            .iter()
            .flat_map(|id| {
                ["spawned", "descendant"].map(|file| self.root.join(format!("apps/{id}/{file}")))
            })
            .filter_map(|path| {
                fs::read_to_string(path)
                    .ok()
                    .map(|pid| pid.parse().unwrap())
            })
            .collect()
    }
}

impl Drop for Workspace {
    fn drop(&mut self) {
        // Keep failed assertions from leaving fixture children behind.
        for pid in self.spawned_pids() {
            if alive(pid) {
                unsafe {
                    libc::kill(pid, libc::SIGKILL);
                }
            }
        }
    }
}

struct Running {
    child: Child,
    stdout: PathBuf,
    stderr: PathBuf,
}

impl Running {
    fn start(mut command: Command, root: &std::path::Path, label: &str) -> Self {
        let label = format!("{label}-{}", uuid::Uuid::new_v4());
        let stdout = root.join(format!("{label}.stdout"));
        let stderr = root.join(format!("{label}.stderr"));
        let child = command
            .stdout(Stdio::from(File::create(&stdout).unwrap()))
            .stderr(Stdio::from(File::create(&stderr).unwrap()))
            .spawn()
            .unwrap();
        Self {
            child,
            stdout,
            stderr,
        }
    }
    fn stdout(&self) -> String {
        fs::read_to_string(&self.stdout).unwrap()
    }
    fn stderr(&self) -> String {
        fs::read_to_string(&self.stderr).unwrap()
    }
    fn logs(&self) -> String {
        format!("stdout:\n{}\nstderr:\n{}", self.stdout(), self.stderr())
    }
    fn signal(&self, signal: i32) {
        assert_eq!(unsafe { libc::kill(self.child.id() as i32, signal) }, 0);
    }
    fn wait(&mut self) -> ExitStatus {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                return status;
            }
            assert!(
                Instant::now() < deadline,
                "supervisor did not exit: {}",
                self.logs()
            );
            thread::sleep(Duration::from_millis(20));
        }
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn alive(pid: i32) -> bool {
    // A killed orphan can remain a zombie until the namespace's init reaps it.
    #[cfg(target_os = "linux")]
    if let Ok(stat) = fs::read_to_string(format!("/proc/{pid}/stat"))
        && stat
            .rsplit_once(") ")
            .is_some_and(|(_, fields)| fields.starts_with('Z'))
    {
        return false;
    }
    unsafe { libc::kill(pid, 0) == 0 }
}

fn in_network_namespace(test: &str) -> bool {
    if std::env::var_os("NOOK_WORKSPACE_NETNS").is_some() {
        assert!(
            Command::new("ip")
                .args(["link", "set", "lo", "up"])
                .status()
                .unwrap()
                .success()
        );
        return false;
    }
    if std::env::var_os("NOOK_E2E_DIRECT").is_some() {
        return false;
    }
    let output = Command::new("unshare")
        .args(["--user", "--map-root-user", "--net"])
        .arg(std::env::current_exe().unwrap())
        .args(["--exact", test, "--nocapture", "--test-threads=1"])
        .env("NOOK_WORKSPACE_NETNS", "1")
        .output()
        .expect("unshare is required; use NOOK_E2E_DIRECT=1 only on an isolated host");
    assert!(
        output.status.success(),
        "namespace test failed:\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    true
}
