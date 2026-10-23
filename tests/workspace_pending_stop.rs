//! An unstarted runner member must never make an individual stop target the runner.
#![cfg(target_os = "linux")]

#[path = "support/caddy.rs"]
mod caddy;

use std::fs::{self, File};
use std::net::TcpListener;
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::Value;

fn managed_routes(config: &str) -> std::collections::BTreeMap<String, Value> {
    fn collect(value: &Value, routes: &mut std::collections::BTreeMap<String, Value>) {
        match value {
            Value::Object(object) => {
                if let Some(id) = object.get("@id").and_then(Value::as_str)
                    && id.starts_with("nook_route_v1_")
                {
                    routes.insert(id.to_owned(), value.clone());
                } else {
                    for value in object.values() {
                        collect(value, routes);
                    }
                }
            }
            Value::Array(array) => {
                for value in array {
                    collect(value, routes);
                }
            }
            _ => {}
        }
    }
    let mut routes = std::collections::BTreeMap::new();
    collect(&serde_json::from_str(config).unwrap(), &mut routes);
    routes
}

const RUNNER: &str = r#"
import os, pathlib, signal, subprocess, sys, time
root = pathlib.Path.cwd()
def stop(sig, frame):
    (root/'runner-signalled').write_text(str(sig))
    sys.exit(0)
signal.signal(signal.SIGTERM, stop)
subprocess.Popen([os.environ['TEST_NOOK'], 'run', '--', sys.executable, str(root/'server.py')], cwd=root/'api')
while True:
    time.sleep(.01)
"#;

const SERVER: &str = r#"
import http.server, os, pathlib, signal, sys
def stop(sig, frame):
    pathlib.Path('stopped').write_text(str(sig))
    sys.exit(0)
signal.signal(signal.SIGTERM, stop)
server = http.server.HTTPServer((os.environ['HOST'], int(os.environ['PORT'])), http.server.SimpleHTTPRequestHandler)
server.serve_forever()
"#;

struct Fixture {
    caddy: caddy::CaddyHarness,
    root: Child,
}

impl Fixture {
    fn command(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_nook"));
        command
            .current_dir(self.caddy.root())
            .env("XDG_CONFIG_HOME", self.caddy.root().join("config"))
            .env("XDG_STATE_HOME", self.caddy.root().join("state"))
            .env("NOOK_DISABLE_UPDATE_CHECK", "1")
            .env_remove("NOOK_WORKSPACE_SESSION");
        command
    }

    fn state(&self) -> Option<Value> {
        fs::read(self.caddy.root().join("state/nook/state.json"))
            .ok()
            .map(|bytes| serde_json::from_slice(&bytes).unwrap())
    }

    fn until(&mut self, mut check: impl FnMut(&Self) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(8);
        while !check(self) {
            assert!(self.root.try_wait().unwrap().is_none(), "workspace exited");
            assert!(
                Instant::now() < deadline,
                "workspace timed out: {}",
                fs::read_to_string(self.caddy.root().join("stderr")).unwrap()
            );
            thread::sleep(Duration::from_millis(10));
        }
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        // Let the workspace reap its separate runner/member groups on assertion failures too.
        unsafe {
            libc::kill(self.root.id() as i32, libc::SIGTERM);
        }
        let deadline = Instant::now() + Duration::from_secs(5);
        while self.root.try_wait().unwrap().is_none() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(10));
        }
        let _ = self.root.kill();
        let _ = self.root.wait();
    }
}

fn pending_stop(test: &str, force: bool) {
    if std::env::var_os("NOOK_PENDING_STOP_NETNS").is_some() {
        assert!(
            Command::new("ip")
                .args(["link", "set", "lo", "up"])
                .status()
                .unwrap()
                .success()
        );
    } else if std::env::var_os("NOOK_E2E_DIRECT").is_none() {
        let output = Command::new("unshare")
            .args(["--user", "--map-root-user", "--net"])
            .arg(std::env::current_exe().unwrap())
            .args(["--exact", test, "--nocapture"])
            .env("NOOK_PENDING_STOP_NETNS", "1")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    let caddy = caddy::CaddyHarness::start_on_standard_ports();
    let root = caddy.root();
    fs::create_dir_all(root.join("config/nook")).unwrap();
    fs::write(
        root.join("config/nook/config.toml"),
        format!(
            "format_version = 1\ncaddy_admin = {:?}\n",
            caddy.admin_url()
        ),
    )
    .unwrap();
    fs::write(root.join("runner.py"), RUNNER).unwrap();
    fs::write(root.join("server.py"), SERVER).unwrap();
    let mut manifest = "format_version = 2\nname = 'pending'\n".to_owned();
    for app in ["api", "web"] {
        fs::create_dir(root.join(app)).unwrap();
        manifest.push_str(&format!("[[apps]]\nid = '{app}'\ndir = '{app}'\ntls = false\nframework = 'none'\ncommand = ['unused']\n"));
    }
    fs::write(root.join("nook.toml"), manifest).unwrap();
    let child = Command::new(env!("CARGO_BIN_EXE_nook"))
        .current_dir(root)
        .env("XDG_CONFIG_HOME", root.join("config"))
        .env("XDG_STATE_HOME", root.join("state"))
        .env("NOOK_DISABLE_UPDATE_CHECK", "1")
        .env("TEST_NOOK", env!("CARGO_BIN_EXE_nook"))
        .env_remove("NOOK_WORKSPACE_SESSION")
        .args(["run", "--runner", "--", "python3", "runner.py"])
        .stdout(Stdio::from(File::create(root.join("stdout")).unwrap()))
        .stderr(Stdio::from(File::create(root.join("stderr")).unwrap()))
        .spawn()
        .unwrap();
    let mut fixture = Fixture { caddy, root: child };
    fixture.until(|fixture| {
        fixture.state().is_some_and(|state| {
            state["leases"].as_object().unwrap().values().any(|lease| {
                lease["hostname"] == "api.pending.localhost" && lease["state"] == "ready"
            })
        })
    });
    let state = fixture.state().unwrap();
    let pending = state["leases"]
        .as_object()
        .unwrap()
        .values()
        .find(|lease| lease["hostname"] == "web.pending.localhost")
        .unwrap();
    let group = &state["workspace_groups"][pending["group_id"].as_str().unwrap()];
    assert_eq!(pending["pid"], group["runner"]["pid"]);
    assert_eq!(pending["workspace_supervisor"], group["supervisor"]);
    let port = pending["target"]
        .as_str()
        .unwrap()
        .rsplit(':')
        .next()
        .unwrap()
        .parse::<u16>()
        .unwrap();
    assert!(
        TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, port)).is_err(),
        "pending port must be reserved"
    );
    // CLI reconciliation may reorder the route array; ownership/content must stay intact.
    let routes = managed_routes(&fixture.caddy.config_json());
    assert_eq!(routes.len(), 2);
    let mut command = fixture.command();
    command.args(["stop", "web.pending.localhost"]);
    if force {
        command.arg("--force");
    }
    let output = command.output().unwrap();
    assert!(
        !output.status.success(),
        "pending stop must be rejected: {output:?}"
    );
    let error = String::from_utf8_lossy(&output.stderr);
    assert!(
        error.contains("not started") || error.contains("pending member association"),
        "{error}"
    );
    thread::sleep(Duration::from_millis(150));
    assert!(fixture.root.try_wait().unwrap().is_none());
    assert!(!fixture.caddy.root().join("runner-signalled").exists());
    assert!(!fixture.caddy.root().join("api/stopped").exists());
    for lease in state["leases"].as_object().unwrap().values() {
        assert_eq!(
            unsafe { libc::kill(lease["pid"].as_u64().unwrap() as i32, 0) },
            0
        );
    }
    assert_eq!(fixture.state().unwrap()["leases"], state["leases"]);
    assert_eq!(managed_routes(&fixture.caddy.config_json()), routes);
    assert!(TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, port)).is_err());
    let mut command = fixture.command();
    command.args(["stop", "api.pending.localhost"]);
    if force {
        command.arg("--force");
    }
    let output = command.output().unwrap();
    assert!(output.status.success(), "{output:?}");
    fixture.until(|fixture| fixture.caddy.root().join("api/stopped").exists());
    assert!(fixture.root.try_wait().unwrap().is_none());
    assert!(!fixture.caddy.root().join("runner-signalled").exists());
}

#[test]
fn pending_member_stop_preserves_runner_and_active_sibling() {
    pending_stop(
        "pending_member_stop_preserves_runner_and_active_sibling",
        false,
    );
}

#[test]
fn pending_member_force_stop_preserves_runner_and_active_sibling() {
    pending_stop(
        "pending_member_force_stop_preserves_runner_and_active_sibling",
        true,
    );
}
