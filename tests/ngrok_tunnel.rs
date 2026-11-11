//! Real Caddy plus an isolated ngrok CLI/API fixture; no public network.
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

const CONNECTOR: &str = include_str!("support/ngrok_api_mock.py");
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
        .env("NGROK_AUTHTOKEN", "fake-test-token")
        .env("NGROK_API_KEY", "fake-api-key")
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
    let binary = root.join("fake-ngrok");
    fs::write(&binary, CONNECTOR).unwrap();
    fs::set_permissions(&binary, fs::Permissions::from_mode(0o700)).unwrap();
    fs::write(root.join("config/nook/config.toml"),format!("format_version = 1\ncaddy_admin = {:?}\n[ngrok]\nbinary = {:?}\nstartup_timeout_seconds = 3\n",c.admin_url(),binary.to_str().unwrap())).unwrap();
    fs::write(root.join("app.py"), APP).unwrap();
    c
}
#[test]
fn ngrok_discovers_owned_url_and_cleans_up_without_touching_caddy() {
    if isolated("ngrok_discovers_owned_url_and_cleans_up_without_touching_caddy") {
        return;
    }
    let foreign = std::net::TcpListener::bind("127.0.0.1:4040").unwrap();
    let c = fixture();
    let root = c.root();
    let original: Value = serde_json::from_str(&c.config_json()).unwrap();
    let mut child = Running(
        nook(root)
            .args([
                "run", "--name", "web", "--tunnel", "ngrok", "--", "python3", "app.py",
            ])
            .env("NOOK_FAKE_MODE", "bind-retry")
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
    let directory = root.join(format!("state/nook/tunnel-{id}"));
    assert!(env.get("NGROK_AUTHTOKEN").is_none());
    assert!(env.get("NGROK_API_KEY").is_none());
    assert_eq!(entry["provider"], "ngrok");
    assert_ne!(entry["metrics_port"], 4040);
    assert!(directory.join("retried").exists());
    assert!(std::net::TcpStream::connect(foreign.local_addr().unwrap()).is_ok());
    let argv = fs::read_to_string(directory.join("argv.json")).unwrap();
    assert!(argv.contains("--inspect=false"));
    assert!(argv.contains("--upstream-tls-verify=true"));
    assert!(!argv.contains("fake-test-token"));
    let policy: Value =
        serde_json::from_slice(&fs::read(directory.join("policy.json")).unwrap()).unwrap();
    assert!(
        policy["on_http_request"][0]["actions"][0]["config"]["headers"]["host"]
            .as_str()
            .unwrap()
            .starts_with("127.0.0.1:")
    );
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
fn ngrok_failure_does_not_spawn_application() {
    if isolated("ngrok_failure_does_not_spawn_application") {
        return;
    }
    let c = fixture();
    let root = c.root();
    for mode in ["exit", "timeout", "foreign", "wrong-upstream", "wrong-url"] {
        let output = nook(root)
            .args([
                "run", "--name", "web", "--tunnel", "ngrok", "--", "python3", "app.py",
            ])
            .env("NOOK_FAKE_MODE", mode)
            .output()
            .unwrap();
        assert!(!output.status.success());
        assert!(!String::from_utf8_lossy(&output.stderr).contains("secret-value"));
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
fn ngrok_workspace_urls_and_alias_lifecycle() {
    if isolated("ngrok_workspace_urls_and_alias_lifecycle") {
        return;
    }
    let c = fixture();
    let root = c.root();
    for app in ["api", "web"] {
        fs::create_dir(root.join(app)).unwrap();
        fs::write(root.join(app).join("app.py"), APP).unwrap();
    }
    fs::write(root.join("nook.toml"), "format_version=2\nname='shop'\n[[apps]]\nid='api'\ndir='api'\ntunnel='ngrok'\nngrok_url='https://api.ngrok.app'\ncommand=['python3','app.py']\n[[apps]]\nid='web'\ndir='web'\ntunnel='ngrok'\nngrok_url='https://web.ngrok.app'\ncommand=['python3','app.py']\n[apps.env]\nAPI_URL='${api.tunnel_url}'\n").unwrap();
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
    assert_eq!(web["API_URL"], "https://api.ngrok.app");
    assert_eq!(web["NOOK_TUNNEL_URL"], "https://web.ngrok.app");
    child.0.kill().unwrap();
    child.0.wait().unwrap();
    until(|| {
        nook(root).arg("prune").output().unwrap();
        state(root)["tunnels"]
            .as_object()
            .is_some_and(|t| t.is_empty())
    });
    assert!(
        nook(root)
            .args(["alias", "set", "api", "3000"])
            .status()
            .unwrap()
            .success()
    );
    let mut alias = Running(nook(root).args(["tunnel", "alias", "api"]).spawn().unwrap());
    until(|| {
        state(root)["tunnels"]
            .as_object()
            .is_some_and(|t| t.values().any(|e| e["phase"] == "connected"))
    });
    let registry = state(root);
    let id = registry["tunnels"]
        .as_object()
        .unwrap()
        .keys()
        .next()
        .unwrap();
    assert!(
        nook(root)
            .args(["tunnel", "stop", id])
            .status()
            .unwrap()
            .success()
    );
    until(|| alias.0.try_wait().unwrap().is_some());
    assert!(state(root)["aliases"].get("api.localhost").is_some());
    assert!(state(root)["tunnels"].as_object().unwrap().is_empty());
}
