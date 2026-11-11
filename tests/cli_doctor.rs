//! Diagnostic safety is verified at the process boundary on Linux and Windows.
use serde_json::{Value, json};
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::path::PathBuf;
use std::process::{Command, Output};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};
use std::time::{Duration, Instant};

#[path = "support/compose_registry.rs"]
mod compose_registry;

struct Fixture {
    root: PathBuf,
    stop: Arc<AtomicBool>,
    requests: Arc<Mutex<Vec<String>>>,
    config: Arc<Mutex<Value>>,
    server: Option<std::thread::JoinHandle<()>>,
}
impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!("nook-doctor-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(root.join("config/nook")).unwrap();
        fs::create_dir_all(root.join("project")).unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        fs::write(
            root.join("config/nook/config.toml"),
            format!(
                "format_version = 1\ncaddy_admin = 'http://{}'\n",
                listener.local_addr().unwrap()
            ),
        )
        .unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let requests = Arc::new(Mutex::new(Vec::new()));
        let config = Arc::new(Mutex::new(
            json!({"apps":{"http":{"servers":{"plain":{"listen":[":80"],"routes":[]},"secure":{"listen":[":443"],"routes":[]}}}}}),
        ));
        let thread_stop = stop.clone();
        let thread_requests = requests.clone();
        let thread_config = config.clone();
        let server = std::thread::spawn(move || {
            while !thread_stop.load(Ordering::Relaxed) {
                let (mut stream, _) = match listener.accept() {
                    Ok(pair) => pair,
                    Err(_) => {
                        std::thread::sleep(Duration::from_millis(2));
                        continue;
                    }
                };
                // Winsock inherits the listener's nonblocking mode; reads below wait for headers.
                stream.set_nonblocking(false).unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(1)))
                    .unwrap();
                let request = read_request_headers(&mut stream)
                    .lines()
                    .next()
                    .unwrap_or("")
                    .to_owned();
                thread_requests.lock().unwrap().push(request.clone());
                let body = if request.starts_with("GET /config/ ") {
                    thread_config.lock().unwrap().to_string()
                } else {
                    "{}".into()
                };
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nETag: snapshot\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = stream.write_all(response.as_bytes());
            }
        });
        Self {
            root,
            stop,
            requests,
            config,
            server: Some(server),
        }
    }
    fn command(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_nook"));
        command
            .current_dir(self.root.join("project"))
            .env("XDG_CONFIG_HOME", self.root.join("config"))
            .env("XDG_STATE_HOME", self.root.join("state"))
            .env("XDG_CACHE_HOME", self.root.join("cache"))
            .env("HOME", &self.root)
            .env("APPDATA", self.root.join("appdata"))
            .env("LOCALAPPDATA", self.root.join("localappdata"))
            .env_remove("NOOK_DISABLE_UPDATE_CHECK")
            .env_remove("NOOK_WORKSPACE_SESSION");
        command
    }
    fn doctor(&self, args: &[&str]) -> (Output, Value) {
        let output = self
            .command()
            .args(["doctor", "--json"])
            .args(args)
            .output()
            .unwrap();
        let json = serde_json::from_slice(&output.stdout)
            .unwrap_or_else(|error| panic!("{error}: {:?}", output));
        assert!(output.stderr.is_empty(), "{:?}", output);
        (output, json)
    }
    fn state(&self) -> PathBuf {
        self.root.join("state/nook/state.json")
    }
    fn write_state(&self, registry: &Value) {
        fs::create_dir_all(self.state().parent().unwrap()).unwrap();
        fs::write(self.state(), registry.to_string()).unwrap();
    }
    fn alias(&self) -> uuid::Uuid {
        let id = uuid::Uuid::new_v4();
        self.write_state(&json!({"format_version":2,"aliases":{"api.localhost":{"id":id,"hostname":"api.localhost","target":"http://127.0.0.1:3000","scheme":"http","tls":false,"preserve_host":false}},"leases":{},"selected_servers":{},"last_synchronized_at_unix_ms":123,"pending_operations":[]}));
        id
    }
    fn assert_read_only(&self) {
        assert!(!self.root.join("cache").exists());
        assert!(!self.state().with_extension("lock").exists());
        assert!(!self.state().with_extension("operations.lock").exists());
        assert!(
            self.requests
                .lock()
                .unwrap()
                .iter()
                .all(|r| r.starts_with("GET /config/ "))
        );
    }
}
fn read_request_headers(stream: &mut std::net::TcpStream) -> String {
    let mut reader = BufReader::new(stream);
    let mut headers = String::new();
    loop {
        let start = headers.len();
        if reader.read_line(&mut headers).unwrap_or(0) == 0 || &headers[start..] == "\r\n" {
            return headers;
        }
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        self.server.take().unwrap().join().unwrap();
        let _ = fs::remove_dir_all(&self.root);
    }
}
fn check<'a>(report: &'a Value, id: &str) -> &'a Value {
    report["checks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["id"] == id)
        .unwrap_or_else(|| panic!("missing {id}: {report}"))
}

#[test]
fn quick_inspection_does_not_initialize_state_update_cache_or_ca() {
    let fixture = Fixture::new();
    let before = fixture.config.lock().unwrap().clone();
    let (output, report) = fixture.doctor(&[]);
    assert_eq!(output.status.code(), Some(0), "{report}");
    assert_eq!(report["schema_version"], 1);
    assert_eq!(
        check(&report, "ca.trust")["reason_code"],
        "certificate_not_supplied"
    );
    assert!(!fixture.root.join("state").exists());
    assert_eq!(*fixture.config.lock().unwrap(), before);
    fixture.assert_read_only();
}

#[test]
fn missing_route_is_reported_before_reconciliation_and_registry_is_unchanged() {
    let fixture = Fixture::new();
    let owner = fixture.alias();
    let before = fs::read(fixture.state()).unwrap();
    let (output, report) = fixture.doctor(&["--owner", &owner.to_string()]);
    assert_eq!(output.status.code(), Some(1), "{report}");
    assert_eq!(
        check(&report, "caddy.route")["reason_code"],
        "expected_route_absent"
    );
    assert_eq!(fs::read(fixture.state()).unwrap(), before);
    fixture.assert_read_only();
}

#[test]
fn independent_errors_continue_and_parse_errors_do_not_echo_secrets() {
    let fixture = Fixture::new();
    fs::write(
        fixture.root.join("config/nook/config.toml"),
        "secret = 'never-print-me'\nformat_version = 'bad'",
    )
    .unwrap();
    fixture.write_state(&json!({"format_version":900}));
    fs::write(
        fixture.root.join("project/nook.toml"),
        "format_version = 1\ncommand = ['server']\n",
    )
    .unwrap();
    let (output, report) = fixture.doctor(&[]);
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(check(&report, "config.global")["status"], "fail");
    assert_eq!(check(&report, "state.snapshot")["status"], "fail");
    assert_eq!(check(&report, "config.project")["status"], "pass");
    assert!(
        !String::from_utf8(output.stdout)
            .unwrap()
            .contains("never-print-me")
    );
    fixture.assert_read_only();
}

#[test]
fn busy_lock_is_bounded_and_other_checks_continue() {
    let fixture = Fixture::new();
    fixture.alias();
    let file = fs::File::create(fixture.state().with_extension("operations.lock")).unwrap();
    file.lock().unwrap();
    let start = Instant::now();
    let (output, report) = fixture.doctor(&[]);
    assert!(start.elapsed() < Duration::from_secs(5));
    assert_eq!(output.status.code(), Some(3), "{report}");
    assert_eq!(check(&report, "state.snapshot")["reason_code"], "lock_busy");
    assert_eq!(check(&report, "caddy.api")["status"], "pass");
}

#[test]
fn json_usage_errors_are_machine_readable() {
    let fixture = Fixture::new();
    for args in [
        &["--timeout", "0s"][..],
        &["--force"],
        &["--local"],
        &["--owner", "bad"],
    ] {
        let (output, report) = fixture.doctor(args);
        assert_eq!(output.status.code(), Some(2));
        assert_eq!(report["exit_code"], 2);
    }
    assert!(fixture.requests.lock().unwrap().is_empty());
}

#[test]
fn simulated_routes_report_foreign_collisions_without_expectation_of_installed_routes() {
    for (server, hostname, flags) in [
        ("plain", "api.localhost", vec!["--no-tls"]),
        ("secure", "api.localhost", vec![]),
        ("plain", "api.local", vec!["--no-tls", "--lan"]),
    ] {
        let fixture = Fixture::new();
        fixture.config.lock().unwrap()["apps"]["http"]["servers"][server]["routes"] = json!([
            {"match":[{"host":[hostname]}],"handle":[{"handler":"static_response","body":"foreign"}]}
        ]);
        let before = fixture.config.lock().unwrap().clone();
        let mut args = vec!["--name", "api"];
        args.extend(flags);
        args.extend(["--", "hypothetical-server"]);
        let (output, report) = fixture.doctor(&args);
        assert_eq!(output.status.code(), Some(1), "{report}");
        assert_eq!(
            check(&report, "caddy.foreign")["reason_code"],
            "foreign_hostname"
        );
        assert!(
            !report["checks"]
                .as_array()
                .unwrap()
                .iter()
                .any(|c| c["reason_code"] == "expected_route_absent")
        );
        assert_eq!(*fixture.config.lock().unwrap(), before);
        assert!(!fixture.root.join("state").exists());
        fixture.assert_read_only();
    }
}

#[test]
fn json_usage_errors_accept_both_global_socket_spellings() {
    let fixture = Fixture::new();
    for prefix in [
        vec!["--caddy-socket=/tmp/caddy.sock"],
        vec!["--caddy-socket", "/tmp/caddy.sock"],
    ] {
        let output = fixture
            .command()
            .args(prefix)
            .args(["doctor", "--json", "--timeout", "0s"])
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(2));
        assert!(output.stderr.is_empty(), "{:?}", output);
        let report: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(report["schema_version"], 1);
        assert_eq!(report["exit_code"], 2);
    }
    assert!(fixture.requests.lock().unwrap().is_empty());
}

#[test]
fn project_simulation_never_executes_the_child_and_preserves_overlay_rules() {
    let fixture = Fixture::new();
    fs::write(fixture.root.join("project/nook.toml"), "format_version = 1\nname = 'base'\ncommand = ['this-command-does-not-exist']\nrun_bind_address = '127.0.0.1'").unwrap();
    fs::write(
        fixture.root.join("project/nook.local.toml"),
        "format_version = 1\nname = 'local'",
    )
    .unwrap();
    for (args, expected) in [
        (&[][..], "local.localhost"),
        (&["--config", "nook.toml"][..], "base.localhost"),
        (&["--config", "nook.toml", "--local"][..], "local.localhost"),
        (&["--name", "cli"][..], "cli.localhost"),
    ] {
        let (output, report) = fixture.doctor(args);
        assert_eq!(output.status.code(), Some(0), "{report}");
        assert_eq!(
            check(&report, "config.project")["resource"]["hostname"],
            expected
        );
    }
    fixture.assert_read_only();
}

#[test]
fn bounded_worker_does_not_follow_redirects_or_get_application_bodies() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        assert!(read_request_headers(&mut stream).starts_with("HEAD / HTTP/1.1"));
        stream.write_all(b"HTTP/1.1 302 Found\r\nLocation: http://127.0.0.1:1/side-effect\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").unwrap();
    });
    let output = Command::new(env!("CARGO_BIN_EXE_nook"))
        .arg("__doctor_probe")
        .arg(
            json!({"Http":{"url":format!("http://{address}/"),"address":null,"ca_cert":null}})
                .to_string(),
        )
        .output()
        .unwrap();
    server.join().unwrap();
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["Ok"]["status"], 302, "{value}");
}

#[test]
fn real_caddy_inspection_never_provisions_local_ca() {
    let fixture = Fixture::new();
    let guard = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = guard.local_addr().unwrap();
    let config_path = fixture.root.join("caddy.json");
    fs::write(&config_path, json!({"admin":{"listen":address.to_string()},"apps":{"http":{"servers":{"plain":{"listen":["127.0.0.1:0"],"routes":[{"handle":[{"handler":"static_response","body":"foreign"}]}]}}}}}).to_string()).unwrap();
    fs::write(
        fixture.root.join("config/nook/config.toml"),
        format!("format_version = 1\ncaddy_admin = 'http://{address}'\n"),
    )
    .unwrap();
    drop(guard);
    struct Child(std::process::Child);
    impl Drop for Child {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let mut child = Child(
        Command::new(if cfg!(windows) { "caddy.exe" } else { "caddy" })
            .args(["run", "--config"])
            .arg(&config_path)
            .env("XDG_DATA_HOME", fixture.root.join("caddy-data"))
            .env("XDG_CONFIG_HOME", fixture.root.join("caddy-config"))
            .env("APPDATA", fixture.root.join("caddy-appdata"))
            .env("LOCALAPPDATA", fixture.root.join("caddy-localappdata"))
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("Caddy is required, as in the existing E2E suites"),
    );
    let deadline = Instant::now() + Duration::from_secs(5);
    let url = format!("http://{address}/config/");
    let read = || {
        ureq::get(&url)
            .call()
            .ok()
            .and_then(|mut r| r.body_mut().read_json::<Value>().ok())
    };
    let before = loop {
        if let Some(config) = read() {
            break config;
        }
        assert!(
            Instant::now() < deadline && child.0.try_wait().unwrap().is_none(),
            "Caddy startup failed"
        );
        std::thread::sleep(Duration::from_millis(10));
    };
    let (output, report) = fixture.doctor(&[]);
    assert_eq!(output.status.code(), Some(0), "{report}");
    assert_eq!(read().unwrap(), before);
    for directory in ["caddy-data", "caddy-appdata", "caddy-localappdata"] {
        assert!(!fixture.root.join(directory).join("caddy/pki").exists());
        assert!(!fixture.root.join(directory).join("Caddy/pki").exists());
    }
    fixture.assert_read_only();
}

#[test]
fn stalled_admin_is_bounded_and_still_reports_independent_checks() {
    let fixture = Fixture::new();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    fs::write(
        fixture.root.join("config/nook/config.toml"),
        format!(
            "format_version = 1\ncaddy_admin = 'http://{}'\n",
            listener.local_addr().unwrap()
        ),
    )
    .unwrap();
    let start = Instant::now();
    let (output, report) = fixture.doctor(&["--timeout", "1s"]);
    assert!(start.elapsed() < Duration::from_secs(3));
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(report["complete"], false);
    assert_eq!(check(&report, "ca.trust")["status"], "skipped");
    fixture.assert_read_only();
}

#[test]
fn legacy_alias_without_id_does_not_generate_a_diagnostic_owner() {
    let fixture = Fixture::new();
    fixture.alias();
    let mut registry: Value = serde_json::from_slice(&fs::read(fixture.state()).unwrap()).unwrap();
    registry["aliases"]["api.localhost"]
        .as_object_mut()
        .unwrap()
        .remove("id");
    fixture.write_state(&registry);
    let (_, report) = fixture.doctor(&[]);
    assert_eq!(check(&report, "state.snapshot")["status"], "fail");
    assert!(
        !report["checks"]
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c["id"] == "caddy.route")
    );
    fixture.assert_read_only();
}

#[cfg(unix)]
#[test]
fn tailscale_observation_never_converges_and_redacts_login_secrets() {
    use std::os::unix::fs::PermissionsExt;
    let fixture = Fixture::new();
    let owner = fixture.alias();
    let mut registry: Value = serde_json::from_slice(&fs::read(fixture.state()).unwrap()).unwrap();
    registry["tailscale"] = json!({"dns_name":"device.example.ts.net", "registrations":{"8443":{"owner_id":owner,"hostname":"api.localhost","target":"http://127.0.0.1:3000","funnel":false}}});
    fixture.write_state(&registry);
    let before = fs::read(fixture.state()).unwrap();
    let script = fixture.root.join("tailscale");
    let log = fixture.root.join("calls");
    fs::write(&script, r#"#!/bin/sh
echo "$*" >> "$DOCTOR_LOG"
case "$*" in
  version) echo '1.90.0' ;;
  'status --json') echo '{"BackendState":"NeedsLogin","AuthURL":"https://secret.invalid/login-token","Self":{"DNSName":"device.example.ts.net"},"CertDomains":["device.example.ts.net"]}' ;;
  'serve status --json') echo '{}' ;;
  *) exit 99 ;;
esac
"#).unwrap();
    fs::set_permissions(&script, fs::Permissions::from_mode(0o700)).unwrap();
    let output = fixture
        .command()
        .env("NOOK_TAILSCALE", &script)
        .env("DOCTOR_LOG", &log)
        .args(["doctor", "--json"])
        .output()
        .unwrap();
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(check(&report, "tailscale.backend")["status"], "fail");
    assert!(
        !String::from_utf8(output.stdout)
            .unwrap()
            .contains("login-token")
    );
    assert_eq!(
        fs::read_to_string(log).unwrap(),
        "version\nstatus --json\nserve status --json\n"
    );
    assert_eq!(fs::read(fixture.state()).unwrap(), before);
    fixture.assert_read_only();
}

#[test]
fn deep_inspection_connects_to_ipv6_upstream() {
    let fixture = Fixture::new();
    let owner = fixture.alias();
    let listener = TcpListener::bind("[::1]:0").unwrap();
    let mut registry: Value = serde_json::from_slice(&fs::read(fixture.state()).unwrap()).unwrap();
    registry["aliases"]["api.localhost"]["target"] =
        json!(format!("http://{}", listener.local_addr().unwrap()));
    fixture.write_state(&registry);
    let before = fs::read(fixture.state()).unwrap();
    let (_, report) = fixture.doctor(&["--owner", &owner.to_string(), "--deep"]);
    assert_eq!(check(&report, "upstream.tcp")["status"], "pass", "{report}");
    assert_eq!(fs::read(fixture.state()).unwrap(), before);
    fixture.assert_read_only();
}

#[cfg(unix)]
#[test]
fn owner_selection_scopes_tailscale_requirements_and_registrations() {
    use std::os::unix::fs::PermissionsExt;
    let fixture = Fixture::new();
    let owner = fixture.alias();
    let other = uuid::Uuid::new_v4();
    let mut registry: Value = serde_json::from_slice(&fs::read(fixture.state()).unwrap()).unwrap();
    registry["tailscale"] = json!({"dns_name":"device.example.ts.net", "registrations":{"8444":{"owner_id":other,"hostname":"other.localhost","target":"http://127.0.0.1:4000","funnel":false}}});
    fixture.write_state(&registry);
    let script = fixture.root.join("tailscale");
    let log = fixture.root.join("calls");
    fs::write(&script, r#"#!/bin/sh
echo "$*" >> "$DOCTOR_LOG"
case "$*" in
  version) echo '1.90.0' ;;
  'status --json') echo '{"BackendState":"Running","Self":{"DNSName":"device.example.ts.net"},"CertDomains":["device.example.ts.net"]}' ;;
  'serve status --json') echo '{"TCP":{"8443":{"HTTPS":true}},"Web":{"device.example.ts.net:8443":{"Handlers":{"/":{"Proxy":"http://127.0.0.1:3000"}}}}}' ;;
  *) exit 99 ;;
esac
"#).unwrap();
    fs::set_permissions(&script, fs::Permissions::from_mode(0o700)).unwrap();
    let inspect = |selected: bool| {
        let mut command = fixture.command();
        command
            .env("NOOK_TAILSCALE", &script)
            .env("DOCTOR_LOG", &log)
            .args(["doctor", "--json"]);
        if selected {
            command.args(["--owner", &owner.to_string()]);
        }
        let output = command.output().unwrap();
        assert!(output.stderr.is_empty());
        serde_json::from_slice::<Value>(&output.stdout).unwrap()
    };
    let before = fs::read(fixture.state()).unwrap();
    let report = inspect(true);
    assert_eq!(
        check(&report, "tailscale")["reason_code"],
        "not_configured",
        "{report}"
    );
    assert!(
        !log.exists(),
        "unselected registration must not invoke Tailscale"
    );
    assert_eq!(fs::read(fixture.state()).unwrap(), before);

    registry["tailscale"]["registrations"]["8443"] = json!({"owner_id":owner,"hostname":"api.localhost","target":"http://127.0.0.1:3000","funnel":false});
    fixture.write_state(&registry);
    let before = fs::read(fixture.state()).unwrap();
    let report = inspect(true);
    let registrations: Vec<_> = report["checks"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|c| c["id"] == "tailscale.registration")
        .collect();
    assert_eq!(registrations.len(), 1, "{report}");
    assert_eq!(registrations[0]["status"], "pass");
    assert_eq!(registrations[0]["evidence"]["owner_id"], owner.to_string());
    let all = inspect(false);
    assert!(
        all["checks"]
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c["id"] == "tailscale.registration"
                && c["evidence"]["owner_id"] == other.to_string()
                && c["reason_code"] == "registration_missing")
    );
    assert_eq!(fs::read(fixture.state()).unwrap(), before);
    fixture.assert_read_only();
}

#[test]
fn fixture_waits_for_complete_request_headers() {
    let fixture = Fixture::new();
    let config = fs::read_to_string(fixture.root.join("config/nook/config.toml")).unwrap();
    let address = config
        .split("http://")
        .nth(1)
        .unwrap()
        .split('\'')
        .next()
        .unwrap();
    let mut stream = std::net::TcpStream::connect(address).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_millis(100)))
        .unwrap();
    stream.write_all(b"GET /config/ HTTP/1.1\r\n").unwrap();
    let error = stream
        .read(&mut [0; 1])
        .expect_err("fixture replied before headers finished");
    assert!(matches!(
        error.kind(),
        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
    ));
    stream.write_all(b"Host: localhost\r\n\r\n").unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).unwrap();
    assert!(response.starts_with("HTTP/1.1 200 OK\r\n"));
    let body = response.split_once("\r\n\r\n").unwrap().1;
    assert_eq!(
        serde_json::from_str::<Value>(body).unwrap(),
        *fixture.config.lock().unwrap()
    );
}

#[test]
fn compose_owners_are_observed_without_docker_or_registry_mutation() {
    let fixture = Fixture::new();
    let owner = uuid::Uuid::new_v4();
    for active in [true, false] {
        fixture.write_state(&compose_registry::registry(
            &fixture.root.join("project"),
            owner,
            active,
        ));
        let before = fs::read(fixture.state()).unwrap();
        let (_, report) = fixture.doctor(&["--owner", &owner.to_string()]);
        let observation = check(&report, "compose.recorded");
        assert_eq!(observation["reason_code"], "docker_not_observed");
        assert_eq!(observation["resource"]["owner_id"], owner.to_string());
        assert_eq!(observation["evidence"]["active"], active);
        let missing = report["checks"]
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c["reason_code"] == "expected_route_absent");
        assert_eq!(missing, active, "{report}");
        assert_eq!(fs::read(fixture.state()).unwrap(), before);
        assert!(!fixture.state().with_extension("pre-compose.json").exists());
        fixture.assert_read_only();
    }
    // A leftover inactive route is recorded drift, never an ownerless route to adopt.
    fixture.config.lock().unwrap()["apps"]["http"]["servers"]["plain"]["routes"] = json!([{
        "@id":"nook_http_routes_v1","handle":[{"handler":"subroute","routes":[{
            "@id":format!("nook_route_v1_{owner}"),"match":[{"host":["web.localhost"]}],
            "handle":[{"handler":"reverse_proxy","upstreams":[{"dial":"127.0.0.1:8080"}]}],"terminal":true
        }]}]
    }]);
    let (_, report) = fixture.doctor(&["--owner", &owner.to_string()]);
    assert_eq!(
        check(&report, "caddy.route")["reason_code"],
        "inactive_compose_route"
    );
    assert!(
        !report["checks"]
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c["id"] == "caddy.unmatched")
    );
    fixture.assert_read_only();
}

#[test]
fn doctor_never_invents_a_compose_owner_or_persists_legacy_migrations() {
    let fixture = Fixture::new();
    let mut registry =
        compose_registry::registry(&fixture.root.join("project"), uuid::Uuid::new_v4(), true);
    registry["compose_projects"]["identity"]["routes"]["web/http"]["alias"]
        .as_object_mut()
        .unwrap()
        .remove("id");
    fixture.write_state(&registry);
    let before = fs::read(fixture.state()).unwrap();
    let (_, report) = fixture.doctor(&[]);
    assert_eq!(check(&report, "state.snapshot")["status"], "fail");
    assert_eq!(fs::read(fixture.state()).unwrap(), before);
    for version in [1, 2] {
        fixture.alias();
        let mut registry: Value =
            serde_json::from_slice(&fs::read(fixture.state()).unwrap()).unwrap();
        registry["format_version"] = json!(version);
        fixture.write_state(&registry);
        let before = fs::read(fixture.state()).unwrap();
        let (_, report) = fixture.doctor(&[]);
        assert_eq!(check(&report, "state.snapshot")["status"], "pass");
        assert_eq!(fs::read(fixture.state()).unwrap(), before);
        assert!(!fixture.state().with_extension("pre-compose.json").exists());
    }
    fixture.assert_read_only();
}
