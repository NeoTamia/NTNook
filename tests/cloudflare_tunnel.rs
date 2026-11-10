//! Deterministic connector lifecycle with a real Caddy TLS origin, no public traffic.
#![cfg(target_os = "linux")]
mod support;
use serde_json::Value;
use std::os::unix::fs::PermissionsExt;
use std::{
    fs,
    path::Path,
    process::{Child, Command, Stdio},
    thread,
    time::{Duration, Instant},
};
use support::caddy::CaddyHarness;

const CONNECTOR: &str = r#"#!/usr/bin/env python3
import http.server,json,os,pathlib,sys,time
args=sys.argv
if '--version' in args:
 print('cloudflared version 2026.10.7');sys.exit(0)
if '--help' in args:
 print('--metrics --pidfile --url --origin-server-name --origin-ca-pool --http-host-header --protocol');sys.exit(0)
root=pathlib.Path(args[args.index('--config')+1]).parent
(root/'argv.json').write_text(json.dumps(args))
(root/'env.json').write_text(json.dumps({k:v for k,v in os.environ.items() if k.startswith('TUNNEL_')}))
mode=os.environ.get('NOOK_FAKE_MODE','ok')
if mode=='exit': sys.exit(7)
port=int(args[args.index('--metrics')+1].split(':')[-1])
class Handler(http.server.BaseHTTPRequestHandler):
 def do_GET(self):
  if (root/'exit').exists(): os._exit(9)
  body=json.dumps({'hostname':'fixture-'+str(os.getpid())+'.trycloudflare.com'}).encode() if self.path=='/quicktunnel' else b'{}'
  self.send_response(503 if mode=='timeout' or (root/'offline').exists() else 200)
  self.end_headers();self.wfile.write(body)
 def log_message(self,*a): pass
server=http.server.HTTPServer(('127.0.0.1',port),Handler)
while mode=='delayed' and not (root/'release').exists(): time.sleep(0.02)
pathlib.Path(args[args.index('--pidfile')+1]).write_text(str(0 if mode=='wrong-pid' else os.getpid()))
server.serve_forever()
"#;
const APP: &str = r#"import http.server,json,os,pathlib
pathlib.Path('app-env.json').write_text(json.dumps(dict(os.environ)))
class Handler(http.server.BaseHTTPRequestHandler):
 def do_GET(self):
  self.send_response(200);self.end_headers();self.wfile.write(json.dumps(dict(self.headers)).encode())
 def log_message(self,*a): pass
http.server.HTTPServer((os.environ['HOST'],int(os.environ['PORT'])),Handler).serve_forever()
"#;
struct Running(Child);
impl Drop for Running {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
fn until(mut condition: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(20);
    while !condition() {
        assert!(Instant::now() < deadline, "condition timed out");
        thread::sleep(Duration::from_millis(50));
    }
}
fn state(root: &Path) -> Value {
    fs::read(root.join("state/nook/state.json"))
        .ok()
        .and_then(|s| serde_json::from_slice(&s).ok())
        .unwrap_or(Value::Null)
}
fn nook(root: &Path) -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_nook"));
    c.current_dir(root)
        .env("XDG_CONFIG_HOME", root.join("config"))
        .env("XDG_STATE_HOME", root.join("state"))
        .env("NOOK_DISABLE_UPDATE_CHECK", "1")
        .env_remove("NOOK_WORKSPACE_SESSION")
        .stdout(Stdio::null());
    c
}
fn isolated(test: &str) -> bool {
    if std::env::var_os("NOOK_CLOUDFLARE_NETNS").is_some() {
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
        .env("NOOK_CLOUDFLARE_NETNS", "1")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    true
}
fn fixture() -> CaddyHarness {
    let c = CaddyHarness::start_on_standard_ports();
    let root = c.root();
    fs::create_dir_all(root.join("config/nook")).unwrap();
    let binary = root.join("fake-cloudflared");
    fs::write(&binary, CONNECTOR).unwrap();
    fs::set_permissions(&binary, fs::Permissions::from_mode(0o700)).unwrap();
    fs::write(root.join("config/nook/config.toml"),format!("format_version = 1\ncaddy_admin = {:?}\n[cloudflare]\nbinary = {:?}\nstartup_timeout_seconds = 3\n",c.admin_url(),binary.to_str().unwrap())).unwrap();
    fs::write(root.join("app.py"), APP).unwrap();
    c
}
#[test]
fn quick_tunnel_routes_tls_and_recovers_without_leaking_ownership() {
    if isolated("quick_tunnel_routes_tls_and_recovers_without_leaking_ownership") {
        return;
    }
    let c = fixture();
    let root = c.root();
    let original: Value = serde_json::from_str(&c.config_json()).unwrap();
    let mut child = Running(
        nook(root)
            .args([
                "run",
                "--name",
                "web",
                "--tunnel",
                "cloudflare",
                "--",
                "python3",
                "app.py",
            ])
            .env("TUNNEL_TOKEN", "must-not-reach-connector")
            .stderr(fs::File::create(root.join("nook.log")).unwrap())
            .spawn()
            .unwrap(),
    );
    until(|| root.join("app-env.json").exists());
    let env: Value = serde_json::from_slice(&fs::read(root.join("app-env.json")).unwrap()).unwrap();
    assert_eq!(env["NOOK_URL"], "https://web.localhost");
    assert!(
        env["NOOK_TUNNEL_URL"]
            .as_str()
            .unwrap()
            .starts_with("https://fixture-")
    );
    let registry = state(root);
    let entry = registry["tunnels"]
        .as_object()
        .unwrap()
        .values()
        .next()
        .unwrap();
    let id = entry["id"].as_str().unwrap();
    let technical = format!("tunnel-{}.localhost", id.replace('-', ""));
    let directory = root.join(format!("state/nook/tunnel-{id}"));
    let argv: Value =
        serde_json::from_slice(&fs::read(directory.join("argv.json")).unwrap()).unwrap();
    assert!(
        !argv
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v == "--no-tls-verify")
    );
    assert_eq!(
        fs::read_to_string(directory.join("env.json")).unwrap(),
        "{}"
    );
    let response = Command::new("curl")
        .args([
            "--silent",
            "--show-error",
            "--fail",
            "--noproxy",
            "*",
            "--resolve",
            &format!("{technical}:443:127.0.0.1"),
            "--cacert",
        ])
        .arg(directory.join("caddy-root.pem"))
        .arg(format!("https://{technical}/"))
        .output()
        .unwrap();
    assert!(
        response.status.success(),
        "{}",
        String::from_utf8_lossy(&response.stderr)
    );
    let headers: Value = serde_json::from_slice(&response.stdout).unwrap();
    assert_eq!(
        headers["Host"],
        env["NOOK_TUNNEL_URL"]
            .as_str()
            .unwrap()
            .strip_prefix("https://")
            .unwrap()
    );
    assert_eq!(headers["X-Forwarded-Host"], headers["Host"]);
    assert_eq!(headers["X-Forwarded-Proto"], "https");
    fs::write(directory.join("exit"), "").unwrap();
    until(|| state(root)["tunnels"][id]["phase"] == "failed");
    assert!(
        child.0.try_wait().unwrap().is_none(),
        "connector failure stopped app"
    );
    assert!(nook(root).args(["stop", "web"]).status().unwrap().success());
    until(|| child.0.try_wait().unwrap().is_some());
    assert!(nook(root).arg("prune").status().unwrap().success());
    assert!(state(root)["tunnels"].as_object().unwrap().is_empty());
    let after: Value = serde_json::from_str(&c.config_json()).unwrap();
    for (name, server) in original["apps"]["http"]["servers"].as_object().unwrap() {
        assert_eq!(
            after["apps"]["http"]["servers"][name]["routes"],
            server["routes"]
        );
    }
}
#[test]
fn quick_tunnel_failure_does_not_spawn_application() {
    if isolated("quick_tunnel_failure_does_not_spawn_application") {
        return;
    }
    let c = fixture();
    let root = c.root();
    for mode in ["exit", "timeout", "wrong-pid"] {
        let output = nook(root)
            .args([
                "run",
                "--name",
                "web",
                "--tunnel",
                "cloudflare",
                "--",
                "python3",
                "app.py",
            ])
            .env("NOOK_FAKE_MODE", mode)
            .output()
            .unwrap();
        assert!(!output.status.success());
        assert!(!root.join("app-env.json").exists());
        assert!(nook(root).arg("prune").status().unwrap().success());
        let registry = state(root);
        assert!(
            registry["tunnels"].as_object().unwrap().is_empty(),
            "{registry}"
        );
        assert!(registry["preparing"].as_object().unwrap().is_empty());
    }
}
#[test]
fn workspace_urls_exist_before_any_application_spawns() {
    if isolated("workspace_urls_exist_before_any_application_spawns") {
        return;
    }
    let c = fixture();
    let root = c.root();
    for app in ["api", "web"] {
        fs::create_dir(root.join(app)).unwrap();
        fs::write(root.join(app).join("app.py"), APP).unwrap();
    }
    fs::write(root.join("nook.toml"),"format_version=2\nname='shop'\n[[apps]]\nid='api'\ndir='api'\ntunnel='cloudflare'\ncommand=['python3','app.py']\n[[apps]]\nid='web'\ndir='web'\ntunnel='cloudflare'\ncommand=['python3','app.py']\n[apps.env]\nAPI_URL='${api.tunnel_url}'\n").unwrap();
    let mut child = Running(
        nook(root)
            .arg("run")
            .stderr(fs::File::create(root.join("nook.log")).unwrap())
            .spawn()
            .unwrap(),
    );
    until(|| root.join("web/app-env.json").exists());
    let web: Value =
        serde_json::from_slice(&fs::read(root.join("web/app-env.json")).unwrap()).unwrap();
    let api: Value =
        serde_json::from_slice(&fs::read(root.join("api/app-env.json")).unwrap()).unwrap();
    assert_eq!(web["API_URL"], api["NOOK_TUNNEL_URL"]);
    assert_ne!(web["NOOK_TUNNEL_URL"], api["NOOK_TUNNEL_URL"]);
    assert!(
        nook(root)
            .args(["stop", "web.shop"])
            .status()
            .unwrap()
            .success()
    );
    until(|| child.0.try_wait().unwrap().is_some());
    assert!(nook(root).arg("prune").status().unwrap().success());
    assert!(state(root)["tunnels"].as_object().unwrap().is_empty());
}

#[test]
fn aliases_and_lan_runs_cannot_replace_routes_reserved_by_starting_runs() {
    if isolated("aliases_and_lan_runs_cannot_replace_routes_reserved_by_starting_runs") {
        return;
    }
    for workspace in [false, true] {
        let c = fixture();
        let root = c.root();
        let config_path = root.join("config/nook/config.toml");
        let config = fs::read_to_string(&config_path).unwrap();
        fs::write(config_path, config.replace("seconds = 3", "seconds = 20")).unwrap();
        if workspace {
            fs::write(root.join("nook.toml"), "format_version=2\nname='shop'\n[[apps]]\nid='web'\ndir='.'\nlan=true\ntunnel='cloudflare'\ncommand=['python3','app.py']\n").unwrap();
        }
        fs::write(root.join("intruder.toml"), "format_version=1\n").unwrap();
        let name = if workspace { "web.shop" } else { "web" };
        let mut command = nook(root);
        command.arg("run");
        if !workspace {
            command.args([
                "--name",
                name,
                "--tunnel",
                "cloudflare",
                "--lan",
                "--",
                "python3",
                "app.py",
            ]);
        }
        let mut child = Running(
            command
                .env("NOOK_FAKE_MODE", "delayed")
                .stderr(Stdio::null())
                .spawn()
                .unwrap(),
        );
        until(|| {
            state(root)["tunnels"]
                .as_object()
                .is_some_and(|t| t.values().any(|e| !e["child"].is_null()))
        });
        let before = c.config_json();
        for force in [true, false] {
            let mut alias = nook(root);
            alias.args(["alias", "set", name, "9000"]);
            if force {
                alias.arg("--force");
            }
            let output = alias.output().unwrap();
            assert!(!output.status.success());
            assert!(
                String::from_utf8_lossy(&output.stderr).contains("reserved by a starting run"),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            let mut collision = nook(root);
            collision.args([
                "run",
                "--config",
                "intruder.toml",
                "--name",
                &format!("{name}.local"),
                "--lan",
            ]);
            if force {
                collision.arg("--force");
            }
            let output = collision
                .args([
                    "--",
                    "python3",
                    "-c",
                    "from pathlib import Path; Path('intruder-started').touch()",
                ])
                .output()
                .unwrap();
            assert!(
                !output.status.success(),
                "colliding LAN run must be rejected"
            );
            assert!(
                String::from_utf8_lossy(&output.stderr).contains("LAN hostname"),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        assert_eq!(c.config_json(), before);
        assert!(state(root)["aliases"].as_object().unwrap().is_empty());
        assert!(!root.join("app-env.json").exists());
        assert!(!root.join("intruder-started").exists());
        let registry = state(root);
        let id = registry["tunnels"]
            .as_object()
            .unwrap()
            .keys()
            .next()
            .unwrap();
        fs::write(root.join(format!("state/nook/tunnel-{id}/release")), "").unwrap();
        until(|| root.join("app-env.json").exists());
        until(|| state(root)["preparing"].as_object().unwrap().is_empty());
        assert_eq!(state(root)["leases"].as_object().unwrap().len(), 1);
        assert!(nook(root).args(["stop", name]).status().unwrap().success());
        until(|| child.0.try_wait().unwrap().is_some());
        assert!(nook(root).arg("prune").status().unwrap().success());
        assert!(state(root)["tunnels"].as_object().unwrap().is_empty());
    }
}

#[test]
fn cancellation_and_supervisor_crash_are_recoverable() {
    if isolated("cancellation_and_supervisor_crash_are_recoverable") {
        return;
    }
    let c = fixture();
    let root = c.root();
    let mut child = Running(
        nook(root)
            .args([
                "run",
                "--name",
                "pending",
                "--tunnel",
                "cloudflare",
                "--",
                "python3",
                "app.py",
            ])
            .env("NOOK_FAKE_MODE", "timeout")
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    until(|| {
        state(root)["tunnels"]
            .as_object()
            .is_some_and(|t| t.values().any(|e| !e["child"].is_null()))
    });
    assert!(
        nook(root)
            .args(["stop", "pending"])
            .status()
            .unwrap()
            .success()
    );
    until(|| child.0.try_wait().unwrap().is_some());
    assert!(!root.join("app-env.json").exists());
    assert!(nook(root).arg("prune").status().unwrap().success());
    let mut child = Running(
        nook(root)
            .args([
                "run",
                "--name",
                "crash",
                "--tunnel",
                "cloudflare",
                "--",
                "python3",
                "app.py",
            ])
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    until(|| root.join("app-env.json").exists());
    child.0.kill().unwrap();
    child.0.wait().unwrap();
    until(|| {
        assert!(nook(root).arg("prune").status().unwrap().success());
        state(root)["tunnels"].as_object().unwrap().is_empty()
    });
    assert!(state(root)["preparing"].as_object().unwrap().is_empty());
}

#[test]
fn force_never_repoints_the_previous_public_url() {
    if isolated("force_never_repoints_the_previous_public_url") {
        return;
    }
    let c = fixture();
    let root = c.root();
    let _old = Running(
        nook(root)
            .args([
                "run",
                "--name",
                "web",
                "--tunnel",
                "cloudflare",
                "--",
                "python3",
                "app.py",
            ])
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    until(|| root.join("app-env.json").exists());
    let old_entry = state(root)["tunnels"]
        .as_object()
        .unwrap()
        .values()
        .next()
        .unwrap()
        .clone();
    fs::remove_file(root.join("app-env.json")).unwrap();
    let mut new = Running(
        nook(root)
            .args([
                "run",
                "--name",
                "web",
                "--force",
                "--tunnel",
                "cloudflare",
                "--",
                "python3",
                "app.py",
            ])
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    until(|| root.join("app-env.json").exists());
    let config = c.config_json();
    let old_host = format!(
        "tunnel-{}.localhost",
        old_entry["id"].as_str().unwrap().replace('-', "")
    );
    assert!(!config.contains(&old_host));
    assert!(nook(root).args(["stop", "web"]).status().unwrap().success());
    until(|| new.0.try_wait().unwrap().is_some());
}

#[test]
fn workspace_runner_inherits_urls_without_spawning_duplicate_connectors() {
    if isolated("workspace_runner_inherits_urls_without_spawning_duplicate_connectors") {
        return;
    }
    let c = fixture();
    let root = c.root();
    fs::write(
        root.join("nook.toml"),
        "format_version=2\nname='runner'\n[[apps]]\nid='web'\ndir='.'\ntunnel='cloudflare'\n",
    )
    .unwrap();
    let mut child = Running(
        nook(root)
            .args([
                "run",
                "--runner",
                "--",
                env!("CARGO_BIN_EXE_nook"),
                "run",
                "--",
                "python3",
                "app.py",
            ])
            .stderr(fs::File::create(root.join("nook.log")).unwrap())
            .spawn()
            .unwrap(),
    );
    until(|| root.join("app-env.json").exists());
    let env: Value = serde_json::from_slice(&fs::read(root.join("app-env.json")).unwrap()).unwrap();
    assert!(
        env["NOOK_TUNNEL_URL"]
            .as_str()
            .unwrap()
            .starts_with("https://fixture-")
    );
    assert_eq!(state(root)["tunnels"].as_object().unwrap().len(), 1);
    assert!(
        nook(root)
            .args(["stop", "web.runner"])
            .status()
            .unwrap()
            .success()
    );
    // Individual member stop may leave the runner alive; ending the root is explicit.
    child.0.kill().unwrap();
    child.0.wait().unwrap();
    until(|| {
        assert!(nook(root).arg("prune").status().unwrap().success());
        state(root)["tunnels"].as_object().unwrap().is_empty()
    });
}
