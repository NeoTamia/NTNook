//! Independent wire client: tests the released binary, not an rmcp client.
use serde_json::{Value, json};
use std::{
    fs,
    io::{BufRead, BufReader, Write},
    path::PathBuf,
    process::{Child, ChildStdin, Command, Stdio},
    sync::mpsc::{self, Receiver},
    thread,
    time::{Duration, Instant},
};
use uuid::Uuid;

#[path = "support/compose_registry.rs"]
mod compose_registry;

struct Fixture {
    root: PathBuf,
}
impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!("nook mcp {}", Uuid::new_v4()));
        fs::create_dir_all(&root).unwrap();
        fs::write(
            root.join("nook.toml"),
            "format_version = 1\nname = 'web'\ntls = false\n",
        )
        .unwrap();
        Self { root }
    }
    fn command(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_nook"));
        command
            .current_dir(&self.root)
            .env("HOME", &self.root)
            .env("XDG_CONFIG_HOME", self.root.join("config"))
            .env("XDG_STATE_HOME", self.root.join("state"))
            .env("APPDATA", self.root.join("config"))
            .env("LOCALAPPDATA", self.root.join("state"))
            .env_remove("NOOK_WORKSPACE_SESSION")
            .env("NOOK_DISABLE_UPDATE_CHECK", "1");
        command
    }
    fn registry(&self) -> PathBuf {
        self.root
            .join("state")
            .join(if cfg!(windows) { "Nook" } else { "nook" })
            .join("state.json")
    }
    fn global(&self, admin: &str) {
        let path = self
            .root
            .join("config")
            .join(if cfg!(windows) { "Nook" } else { "nook" });
        fs::create_dir_all(&path).unwrap();
        fs::write(
            path.join("config.toml"),
            format!("format_version = 1\ncaddy_admin = '{admin}'\n"),
        )
        .unwrap();
    }
    fn spawn(&self, options: &[&str]) -> Client {
        let child = self
            .command()
            .arg("mcp")
            .args(options)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        Client::new(child)
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}
struct Client {
    child: Child,
    input: Option<ChildStdin>,
    output: Receiver<Value>,
    next_id: u64,
    modern: bool,
    schemas: std::collections::BTreeMap<String, Value>,
}
impl Client {
    fn new(mut child: Child) -> Self {
        let input = child.stdin.take();
        let stdout = child.stdout.take().unwrap();
        let (tx, output) = mpsc::channel();
        thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let line = line.unwrap();
                let value = serde_json::from_str(&line)
                    .unwrap_or_else(|e| panic!("non-protocol stdout: {line}: {e}"));
                if tx.send(value).is_err() {
                    break;
                }
            }
        });
        Self {
            child,
            input,
            output,
            next_id: 0,
            modern: false,
            schemas: Default::default(),
        }
    }
    fn send(&mut self, value: Value) {
        writeln!(self.input.as_mut().unwrap(), "{value}").unwrap();
        self.input.as_mut().unwrap().flush().unwrap();
    }
    fn request(&mut self, method: &str, mut params: Value) -> Value {
        self.next_id += 1;
        if self.modern {
            params["_meta"] = json!({"io.modelcontextprotocol/protocolVersion":"2026-07-28","io.modelcontextprotocol/clientCapabilities":{}});
        }
        let id = self.next_id;
        self.send(json!({"jsonrpc":"2.0","id":id,"method":method,"params":params}));
        loop {
            let value = self
                .output
                .recv_timeout(Duration::from_secs(20))
                .expect("bounded MCP response");
            if value["id"] == id {
                return value;
            }
        }
    }
    fn initialize(&mut self, version: &str) {
        if version == "2026-07-28" {
            self.modern = true;
            let response = self.request("server/discover", json!({}));
            assert!(response.get("error").is_none(), "{response}");
        } else {
            let response = self.request("initialize", json!({"protocolVersion":version,"capabilities":{},"clientInfo":{"name":"nook-wire-test","version":"1"}}));
            assert_eq!(response["result"]["protocolVersion"], version, "{response}");
            self.send(json!({"jsonrpc":"2.0","method":"notifications/initialized"}));
        }
    }
    fn call(&mut self, tool: &str, arguments: Value) -> Value {
        if self.schemas.is_empty() {
            let catalog = self.request("tools/list", json!({}));
            for tool in catalog["result"]["tools"].as_array().unwrap() {
                assert_eq!(
                    tool["inputSchema"]["type"], "object",
                    "input schema must declare its root object type"
                );
                assert_eq!(
                    tool["outputSchema"]["type"], "object",
                    "legacy clients require an explicit root object type even with anyOf"
                );
                jsonschema::validator_for(&tool["inputSchema"]).unwrap();
                self.schemas.insert(
                    tool["name"].as_str().unwrap().to_owned(),
                    tool["outputSchema"].clone(),
                );
            }
        }

        let response = self.request("tools/call", json!({"name":tool,"arguments":arguments}));
        assert!(response.get("error").is_none(), "{response}");
        let value = response["result"]["structuredContent"].clone();
        jsonschema::validator_for(&self.schemas[tool])
            .unwrap()
            .validate(&value)
            .unwrap_or_else(|error| panic!("output schema mismatch: {error}: {value}"));
        assert_eq!(
            serde_json::from_str::<Value>(
                response["result"]["content"][0]["text"].as_str().unwrap()
            )
            .unwrap(),
            value
        );
        assert_eq!(response["result"]["isError"], value["ok"] != true);
        value
    }
    fn close(mut self) {
        self.input.take();
        let deadline = Instant::now() + Duration::from_secs(12);
        loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                assert!(status.success());
                return;
            }
            assert!(Instant::now() < deadline, "MCP did not exit after EOF");
            thread::sleep(Duration::from_millis(20));
        }
    }
}
impl Drop for Client {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[test]
fn protocol_versions_read_without_creating_state_or_contacting_caddy() {
    for version in ["2026-07-28", "2025-11-25", "2025-06-18"] {
        let fixture = Fixture::new();
        fixture.global("http://127.0.0.1:1");
        let mut client = fixture.spawn(&[]);
        client.initialize(version);
        let tools = client.request("tools/list", json!({}));
        assert_eq!(tools["result"]["tools"].as_array().unwrap().len(), 4);
        let list = client.call("nook_list", json!({}));
        assert_eq!(list["data"]["entries"], json!([]));
        let url = client.call("nook_url", json!({"mode":"configured"}));
        assert_eq!(url["data"]["url"], "http://web.localhost");
        assert_eq!(url["data"]["registered"], false);
        assert_eq!(
            client.call("nook_url", json!({}))["error"]["code"],
            "NOT_FOUND"
        );
        assert_eq!(
            client.call("nook_status", json!({}))["data"]["health"],
            "error"
        );
        assert!(
            !fixture.root.join("state").exists(),
            "read-only calls created state"
        );
        for (name, args) in [
            (
                "nook_alias_set",
                json!({"hostname":"web.localhost","target":3000}),
            ),
            ("nook_list", json!({"extra_args":["--force"]})),
            ("nook_list", json!({"limit":0})),
            (
                "nook_url",
                json!({"mode":"configured","hostname":"web.localhost"}),
            ),
            ("nook_status", json!({"timeout_ms":0})),
        ] {
            let result = client.request("tools/call", json!({"name":name,"arguments":args}));
            assert!(result.get("error").is_some(), "{result}");
        }
        client.close();
    }
}

#[test]
fn invalid_configuration_and_registry_are_diagnosable_not_silently_empty() {
    let fixture = Fixture::new();
    fixture.global("http://127.0.0.1:1");
    fs::write(fixture.root.join("nook.toml"), "not valid toml !").unwrap();
    let path = fixture.registry();
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, "broken").unwrap();
    let mut client = fixture.spawn(&[]);
    client.initialize("2025-11-25");
    assert_eq!(
        client.call("nook_list", json!({}))["error"]["code"],
        "STATE_INVALID"
    );
    assert_eq!(
        client.call("nook_url", json!({"mode":"configured"}))["error"]["code"],
        "CONFIG_INVALID"
    );
    assert_eq!(
        client.call("nook_tools_profiles", json!({}))["data"]["active_profiles"],
        json!(["read"])
    );
    let status = client.call("nook_status", json!({"detail":"full"}));
    assert_eq!(status["ok"], true);
    assert_eq!(status["data"]["health"], "error");
    assert_eq!(fs::read_to_string(path).unwrap(), "broken");
    client.close();
}

#[test]
fn cli_json_and_commandless_workspace_urls_share_the_api() {
    let fixture = Fixture::new();
    fs::create_dir(fixture.root.join("api")).unwrap();
    fs::create_dir(fixture.root.join("web")).unwrap();
    fs::write(fixture.root.join("nook.toml"), "format_version = 2\nname = 'shop'\n[[apps]]\nid = 'api'\ndir = 'api'\n[[apps]]\nid = 'web'\ndir = 'web'\n").unwrap();
    let output = fixture
        .command()
        .args(["url", "--app", "web", "--mode", "configured", "--json"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        serde_json::from_slice::<Value>(&output.stdout).unwrap()["data"]["url"],
        "https://web.shop.localhost"
    );
    let output = fixture.command().args(["list", "--json"]).output().unwrap();
    assert_eq!(output.status.code(), Some(2));
    let output = fixture
        .command()
        .args(["list", "--read-only", "--json"])
        .output()
        .unwrap();
    assert!(output.status.success());
    assert!(!fixture.root.join("state").exists());
}

#[test]
fn alias_tools_cannot_take_an_inactive_compose_reservation() {
    let fixture = Fixture::new();
    fixture.global("http://127.0.0.1:1");
    let owner = Uuid::new_v4().to_string();
    let registry =
        compose_registry::registry(&fixture.root, Uuid::parse_str(&owner).unwrap(), false);
    let path = fixture.registry();
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, registry.to_string()).unwrap();
    let original = fs::read(&path).unwrap();
    let mut client = fixture.spawn(&["--tools", "read,aliases", "--allow-alias", "web.localhost"]);
    client.initialize("2025-11-25");
    for (tool, input, code) in [
        (
            "nook_alias_set",
            json!({"hostname":"web.localhost","target":3000}),
            "CONFLICT",
        ),
        (
            "nook_alias_remove",
            json!({"hostname":"web.localhost","expected_owner_id":owner}),
            "OWNERSHIP_CHANGED",
        ),
    ] {
        let result = client.call(tool, input);
        assert_eq!(result["error"]["code"], code, "{result}");
        assert_eq!(fs::read(&path).unwrap(), original);
    }
    client.close();
}

#[test]
fn compose_observations_are_read_only_and_scoped_to_the_recorded_root() {
    let fixture = Fixture::new();
    fixture.global("http://127.0.0.1:1");
    let owner = Uuid::new_v4();
    let mut registry = compose_registry::registry(&fixture.root, owner, false);
    let tunnel = Uuid::new_v4();
    registry["tunnels"] = json!({tunnel.to_string(): {
        "id":tunnel,"owner_id":tunnel,"hostname":"public.localhost",
        "target":"http://127.0.0.1:9000/",
        "supervisor":{"pid":std::process::id(),"process_start_time_ticks":1},
        "child":null,"url":"https://fixture.trycloudflare.com",
        "metrics_port":1234,"phase":"connected","committed":false
    }});
    registry["preparing"] = json!({tunnel.to_string(): {
        "hostname":"public.localhost", "lan":false,"tls":true,"cancelled":false,
        "supervisor":{"pid":std::process::id(),"process_start_time_ticks":1}
    }});
    let path = fixture.registry();
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, registry.to_string()).unwrap();
    let original = fs::read(&path).unwrap();
    let mut client = fixture.spawn(&[]);
    client.initialize("2025-11-25");
    let list = client.call("nook_list", json!({"kind":"compose"}));
    let entries = list["data"]["entries"].as_array().unwrap();
    assert_eq!(entries.len(), 1, "{list}");
    let entry = &entries[0];
    assert_eq!(entry["resource_id"], owner.to_string());
    assert_eq!(entry["kind"], "compose");
    assert_eq!(entry["liveness"], "unknown");
    assert_eq!(entry["compose"]["active"], false);
    assert_eq!(entry["compose"]["service"], "web");
    assert_eq!(entry["compose"]["port"], 8080);
    assert_eq!(
        client.call("nook_list", json!({"kind":"alias"}))["data"]["entries"],
        json!([])
    );
    assert_eq!(
        client.call("nook_url", json!({"hostname":"web.localhost"}))["data"]["url"],
        "http://web.localhost"
    );
    let status = client.call("nook_status", json!({}));
    let live = status["data"]["checks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["id"] == "process_liveness")
        .unwrap();
    assert_eq!(live["status"], "unknown");
    assert_eq!(fs::read(&path).unwrap(), original);
    assert!(!path.with_extension("pre-compose.json").exists());
    assert!(!path.with_extension("pre-v6.json").exists());
    client.close();

    // A matching hostname is not sufficient to expose another worktree's project.
    registry["compose_projects"]["identity"]["root"] = json!(fixture.root.join("other"));
    fs::write(&path, registry.to_string()).unwrap();
    let mut scoped = fixture.spawn(&[]);
    scoped.initialize("2025-11-25");
    assert_eq!(
        scoped.call("nook_list", json!({"kind":"compose"}))["data"]["entries"],
        json!([])
    );
    scoped.close();
    let mut all = fixture.spawn(&["--scope", "user"]);
    all.initialize("2025-11-25");
    assert_eq!(
        all.call("nook_list", json!({"kind":"compose"}))["data"]["entries"][0]["resource_id"],
        owner.to_string()
    );
    all.close();
}

#[test]
fn alias_profile_requires_explicit_grants() {
    let fixture = Fixture::new();
    for args in [
        vec!["mcp", "--tools", "aliases"],
        vec!["mcp", "--tools", "read,aliases"],
        vec![
            "mcp",
            "--tools",
            "read,aliases",
            "--allow-alias",
            "*.localhost",
        ],
    ] {
        let output = fixture
            .command()
            .args(args)
            .stdin(Stdio::null())
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(2));
        assert!(output.stdout.is_empty());
    }
    let mut client = fixture.spawn(&["--tools", "read,aliases", "--allow-alias", "db.localhost"]);
    client.initialize("2025-11-25");
    assert_eq!(
        client.request("tools/list", json!({}))["result"]["tools"]
            .as_array()
            .unwrap()
            .len(),
        6
    );
    assert_eq!(
        client.call(
            "nook_alias_set",
            json!({"hostname":"other.localhost","target":3000})
        )["error"]["code"],
        "OUT_OF_SCOPE"
    );
    assert!(!fixture.root.join("state").exists());
    client.close();
}

#[test]
fn alias_creation_preserves_a_starting_runs_reservation() {
    let fixture = Fixture::new();
    fixture.global("http://127.0.0.1:0");
    let owner = Uuid::new_v4().to_string();
    let registry = json!({
        "format_version":3,
        "last_synchronized_at_unix_ms":null,
        "preparing": {owner: {
            "hostname":"web.localhost", "lan":false, "tls":true, "cancelled":false,
            "supervisor":{"pid":std::process::id(),"process_start_time_ticks":1}
        }}
    });
    fs::create_dir_all(fixture.registry().parent().unwrap()).unwrap();
    let before = serde_json::to_vec(&registry).unwrap();
    fs::write(fixture.registry(), &before).unwrap();
    let mut client = fixture.spawn(&["--tools", "read,aliases", "--allow-alias", "web.localhost"]);
    client.initialize("2025-11-25");
    assert_eq!(
        client.call(
            "nook_alias_set",
            json!({"hostname":"web.localhost","target":3000})
        )["error"]["code"],
        "CONFLICT"
    );
    assert_eq!(fs::read(fixture.registry()).unwrap(), before);
    client.close();
}

#[test]
fn real_caddy_aliases_preserve_foreign_routes_and_ownership() {
    #[cfg(target_os = "linux")]
    if std::env::var_os("NOOK_E2E_DIRECT").is_none() && std::env::var_os("NOOK_MCP_NETNS").is_none()
    {
        let output = Command::new("unshare")
            .args(["--user", "--map-root-user", "--net"])
            .arg(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "real_caddy_aliases_preserve_foreign_routes_and_ownership",
                "--nocapture",
            ])
            .env("NOOK_MCP_NETNS", "1")
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
    #[cfg(target_os = "linux")]
    if std::env::var_os("NOOK_MCP_NETNS").is_some() {
        assert!(
            Command::new("ip")
                .args(["link", "set", "lo", "up"])
                .status()
                .unwrap()
                .success()
        );
    }
    if cfg!(windows) {
        // Hosted Windows reserves port 80 (Winsock 10013). Use the same native
        // HTTPS listener as the existing Windows Caddy integration tests.
        caddy_alias_lifecycle(true);
    } else {
        caddy_alias_lifecycle(false);
        caddy_alias_lifecycle(true);
    }
}

fn caddy_alias_lifecycle(tls: bool) {
    let fixture = Fixture::new();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let admin = format!("http://{}", listener.local_addr().unwrap());
    drop(listener);
    fixture.global(&admin);
    let foreign = json!({"@id":"foreign-test","match":[{"host":["foreign.localhost"]}],"handle":[{"handler":"static_response","body":"foreign stays"}],"terminal":true});
    let frontend = if tls { "127.0.0.1:443" } else { "127.0.0.1:80" };
    let mut config = json!({"admin":{"listen":admin.trim_start_matches("http://")},"apps":{"http":{"servers":{"http":{"listen":[frontend],"routes":[foreign.clone()]}}}}});
    if tls {
        config["apps"]["http"]["servers"]["http"]["automatic_https"] =
            json!({"disable_redirects":true});
        config["apps"]["pki"] =
            json!({"certificate_authorities":{"local":{"install_trust":false}}});
        config["apps"]["tls"] = json!({"automation":{"policies":[{"subjects":["db.localhost","foreign.localhost"],"issuers":[{"module":"internal"}]}]}});
    }
    let path = fixture.root.join("caddy.json");
    fs::write(&path, config.to_string()).unwrap();
    struct Caddy(Child);
    impl Drop for Caddy {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let log_path = fixture.root.join("caddy-startup.log");
    let log = fs::File::create(&log_path).unwrap();
    let http_guard = std::net::TcpListener::bind(frontend)
        .expect("isolated Caddy test requires an available frontend listener");
    drop(http_guard);
    let mut caddy = Caddy(
        Command::new(if cfg!(windows) { "caddy.exe" } else { "caddy" })
            .args(["run", "--config"])
            .arg(path)
            .env("XDG_CONFIG_HOME", fixture.root.join("caddy-config"))
            .env("XDG_DATA_HOME", fixture.root.join("caddy-data"))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::from(log))
            .spawn()
            .unwrap(),
    );
    let endpoint = format!("{admin}/config/");
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if ureq::get(&endpoint).call().is_ok() {
            break;
        }
        let status = caddy.0.try_wait().unwrap();
        assert!(
            status.is_none(),
            "Caddy exited ({status:?}): {}",
            fs::read_to_string(&log_path).unwrap_or_default()
        );
        assert!(
            Instant::now() < deadline,
            "Caddy startup timed out: {}",
            fs::read_to_string(&log_path).unwrap_or_default()
        );
        thread::sleep(Duration::from_millis(30));
    }
    let mut client = fixture.spawn(&[
        "--tools",
        "read,aliases",
        "--allow-alias",
        "db.localhost",
        "--allow-alias",
        "foreign.localhost",
    ]);
    client.initialize("2025-11-25");
    assert_eq!(
        client.call(
            "nook_alias_set",
            json!({"hostname":"foreign.localhost","target":3000,"tls":tls})
        )["error"]["code"],
        "FOREIGN_ROUTE"
    );
    let backend = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = backend.local_addr().unwrap().port();
    backend.set_nonblocking(true).unwrap();
    let worker = thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            match backend.accept() {
                Ok((mut stream, _)) => {
                    use std::io::Read;
                    stream
                        .set_read_timeout(Some(Duration::from_secs(2)))
                        .unwrap();
                    let mut buffer = [0; 8192];
                    let _ = stream.read(&mut buffer);
                    stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 7\r\nConnection: close\r\n\r\nmcp-e2e").unwrap();
                    break;
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    assert!(Instant::now() < deadline);
                    thread::sleep(Duration::from_millis(10));
                }
                Err(e) => panic!("{e}"),
            }
        }
    });
    let args = json!({"hostname":"db.localhost","target":port,"tls":tls,"preserve_host":true});
    let created = client.call("nook_alias_set", args.clone());
    assert_eq!(created["ok"], true, "{created}");
    let owner = created["data"]["entry"]["resource_id"].clone();
    let same = client.call("nook_alias_set", args);
    assert_eq!(same["data"]["changed"], false, "{same}");
    assert_eq!(same["data"]["entry"]["resource_id"], owner);
    if tls {
        // Trust only this fixture CA for this curl request; never install it.
        let ca: Value = ureq::get(format!("{admin}/pki/ca/local"))
            .call()
            .unwrap()
            .body_mut()
            .read_json()
            .unwrap();
        let ca_path = fixture.root.join("root.pem");
        fs::write(&ca_path, ca["root_certificate"].as_str().unwrap()).unwrap();
        let mut curl = Command::new(if cfg!(windows) { "curl.exe" } else { "curl" });
        if cfg!(windows) {
            // The ephemeral Caddy CA has no CRL distribution point. Schannel
            // must still verify the chain and hostname against root.pem.
            curl.arg("--ssl-revoke-best-effort");
        }
        let output = curl
            .args([
                "--silent",
                "--show-error",
                "--fail",
                "--noproxy",
                "*",
                "--connect-timeout",
                "5",
                "--max-time",
                "10",
                "--resolve",
                "db.localhost:443:127.0.0.1",
                "--cacert",
            ])
            .arg(ca_path)
            .arg("https://db.localhost/")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(output.stdout, b"mcp-e2e");
    } else {
        let mut response = ureq::get("http://127.0.0.1/")
            .header("Host", "db.localhost")
            .call()
            .unwrap();
        assert_eq!(response.body_mut().read_to_string().unwrap(), "mcp-e2e");
    }
    worker.join().unwrap();
    // Simulate a crash after persisting alias intent, before applying the route.
    let mut interrupted: Value =
        serde_json::from_slice(&fs::read(fixture.registry()).unwrap()).unwrap();
    let operation = Uuid::new_v4().to_string();
    interrupted["pending_operations"] = json!([{"id":operation,"kind":{"kind":"restore_route","hostname":"db.localhost","target":format!("http://127.0.0.1:{port}/"),"scheme":"http","owner_id":owner,"tls":tls}}]);
    fs::write(fixture.registry(), interrupted.to_string()).unwrap();
    ureq::patch(format!("{admin}/config/apps/http/servers/http/routes"))
        .send_json(json!([foreign.clone()]))
        .unwrap();
    let recovered = client.call(
        "nook_alias_set",
        json!({"hostname":"db.localhost","target":port,"tls":tls,"preserve_host":true}),
    );
    assert_eq!(recovered["ok"], true, "{recovered}");
    assert_eq!(recovered["data"]["entry"]["resource_id"], owner);
    assert_eq!(recovered["data"]["operation_id"], operation);
    let state_before = fs::read(fixture.registry()).unwrap();
    let caddy_before: Value = ureq::get(&endpoint)
        .call()
        .unwrap()
        .body_mut()
        .read_json()
        .unwrap();
    let listed = client.call("nook_list", json!({}));
    assert_eq!(listed["data"]["entries"].as_array().unwrap().len(), 1);
    assert_eq!(
        client.call("nook_url", json!({"hostname":"db.localhost"}))["data"]["url"],
        if tls {
            "https://db.localhost"
        } else {
            "http://db.localhost"
        }
    );
    client.call("nook_status", json!({"detail":"full"}));
    assert_eq!(fs::read(fixture.registry()).unwrap(), state_before);
    let observed: Value = ureq::get(&endpoint)
        .call()
        .unwrap()
        .body_mut()
        .read_json()
        .unwrap();
    assert_eq!(observed, caddy_before);
    // The same installed route can belong to Compose rather than a manual alias.
    // Observation must recognize that owner without calling Docker or reconciling.
    let mut compose_state = compose_registry::registry(
        &fixture.root,
        Uuid::parse_str(owner.as_str().unwrap()).unwrap(),
        true,
    );
    let saved_state: Value = serde_json::from_slice(&state_before).unwrap();
    compose_state["compose_projects"]["identity"]["routes"]["web/http"]["alias"] =
        saved_state["aliases"]["db.localhost"].clone();
    for (active, changed_host, changed_tls, expected) in [
        (true, false, false, "ok"),
        (false, false, false, "warning"),
        (false, true, false, "warning"),
        (false, false, true, "warning"),
    ] {
        compose_state["compose_projects"]["identity"]["routes"]["web/http"]["active"] =
            json!(active);
        let alias =
            &mut compose_state["compose_projects"]["identity"]["routes"]["web/http"]["alias"];
        alias["hostname"] = json!(if changed_host {
            "changed.localhost"
        } else {
            "db.localhost"
        });
        alias["tls"] = json!(if changed_tls { !tls } else { tls });
        fs::write(fixture.registry(), compose_state.to_string()).unwrap();
        let before = fs::read(fixture.registry()).unwrap();
        let status = client.call("nook_status", json!({}));
        let routes = status["data"]["checks"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["id"] == "managed_routes")
            .unwrap();
        assert_eq!(routes["status"], expected, "{status}");
        if !active {
            assert_eq!(routes["affected_resource_ids"], json!([owner]));
        }
        assert_eq!(fs::read(fixture.registry()).unwrap(), before);
        let observed: Value = ureq::get(&endpoint)
            .call()
            .unwrap()
            .body_mut()
            .read_json()
            .unwrap();
        assert_eq!(observed, caddy_before);
    }
    ureq::patch(format!("{admin}/config/apps/http/servers/http/routes"))
        .send_json(json!([foreign.clone()]))
        .unwrap();
    let status = client.call("nook_status", json!({}));
    let routes = status["data"]["checks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["id"] == "managed_routes")
        .unwrap();
    assert_eq!(routes["status"], "ok", "{status}");
    ureq::post(format!("{admin}/load"))
        .send_json(&caddy_before)
        .unwrap();
    fs::write(fixture.registry(), &state_before).unwrap();
    let wrong = client.call(
        "nook_alias_remove",
        json!({"hostname":"db.localhost","expected_owner_id":Uuid::new_v4().to_string()}),
    );
    assert_eq!(wrong["error"]["code"], "OWNERSHIP_CHANGED");
    assert_eq!(
        client.call(
            "nook_alias_set",
            json!({"hostname":"db.localhost","target":9999,"tls":tls})
        )["error"]["code"],
        "CONFLICT"
    );
    client.close(); // Persistent aliases survive client shutdown.
    assert_eq!(fs::read(fixture.registry()).unwrap(), state_before);
    let mut client = fixture.spawn(&["--tools", "read,aliases", "--allow-alias", "db.localhost"]);
    client.initialize("2026-07-28");
    // Simulate a removal interrupted after journaling but before Caddy deletion.
    let mut interrupted: Value =
        serde_json::from_slice(&fs::read(fixture.registry()).unwrap()).unwrap();
    interrupted["aliases"]
        .as_object_mut()
        .unwrap()
        .remove("db.localhost");
    interrupted["pending_operations"] = json!([{"id":Uuid::new_v4().to_string(),"kind":{"kind":"remove_route","hostname":"db.localhost","owner_id":owner,"tls":tls}}]);
    fs::write(fixture.registry(), interrupted.to_string()).unwrap();
    let removed = client.call(
        "nook_alias_remove",
        json!({"hostname":"db.localhost","expected_owner_id":owner}),
    );
    assert_eq!(removed["data"]["changed"], true, "{removed}");
    assert_eq!(
        client.call(
            "nook_alias_remove",
            json!({"hostname":"db.localhost","expected_owner_id":owner})
        )["data"]["changed"],
        false
    );
    let final_config: Value = ureq::get(&endpoint)
        .call()
        .unwrap()
        .body_mut()
        .read_json()
        .unwrap();
    assert_eq!(
        final_config["apps"]["http"]["servers"]["http"]["routes"],
        json!([foreign])
    );
    client.close();
    assert!(caddy.0.try_wait().unwrap().is_none());
}

#[test]
fn lock_timeout_cancellation_and_protocol_errors_do_not_poison_connection() {
    let fixture = Fixture::new();
    let path = fixture.registry();
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let lock = fs::File::create(path.with_extension("lock")).unwrap();
    lock.lock().unwrap();
    let mut client = fixture.spawn(&[]);
    client.initialize("2025-11-25");
    let start = Instant::now();
    let result = client.call("nook_list", json!({"timeout_ms":100}));
    assert!(
        matches!(
            result["error"]["code"].as_str(),
            Some("TIMEOUT" | "STATE_BUSY")
        ),
        "{result}"
    );
    assert!(start.elapsed() < Duration::from_secs(2));
    client.send(json!({"jsonrpc":"2.0","id":"cancel-me","method":"tools/call","params":{"name":"nook_list","arguments":{"timeout_ms":30000}}}));
    client.send(json!({"jsonrpc":"2.0","method":"notifications/cancelled","params":{"requestId":"cancel-me","reason":"test"}}));
    thread::sleep(Duration::from_millis(100));
    drop(lock);
    assert_eq!(client.call("nook_list", json!({}))["ok"], true);
    writeln!(client.input.as_mut().unwrap(), "not-json").unwrap();
    // The official SDK ignores unparsable lines; valid JSON with an invalid
    // JSON-RPC shape receives an Invalid Request response.
    client.send(json!({"jsonrpc":"2.0","id":null,"method":42}));
    let error = client.output.recv_timeout(Duration::from_secs(2)).unwrap();
    assert!(error.get("error").is_some(), "{error}");
    assert_eq!(client.call("nook_tools_profiles", json!({}))["ok"], true);
    client.close();
    assert!(!path.exists());
}

#[test]
fn worktree_scope_uses_identity_and_pagination_detects_state_changes() {
    let fixture = Fixture::new();
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
    git(&[
        "-c",
        "user.name=Nook Test",
        "-c",
        "user.email=test@example.invalid",
        "commit",
        "--allow-empty",
        "-m",
        "test",
    ]);
    let other = fixture.root.join("other");
    git(&["worktree", "add", "-b", "other", other.to_str().unwrap()]);
    fs::write(
        other.join("nook.toml"),
        "format_version = 1\nname = 'web'\ntls = false\n",
    )
    .unwrap();
    let mut first = fixture.spawn(&["--worktree=one"]);
    first.initialize("2025-11-25");
    let mut second = fixture.spawn(&["--cwd", other.to_str().unwrap(), "--worktree=two"]);
    second.initialize("2025-11-25");
    let one = first.call("nook_tools_profiles", json!({}))["context"]["worktree_key"].clone();
    let two = second.call("nook_tools_profiles", json!({}))["context"]["worktree_key"].clone();
    assert_ne!(one, two);
    let a = Uuid::new_v4().to_string();
    let b = Uuid::new_v4().to_string();
    let lease = |id: &str, key: Value, label: &str| json!({"id":id,"hostname":"shared.localhost","target":"http://127.0.0.1:3000/","scheme":"http","tls":false,"pid":4294967294u32,"pgid":2147483646,"process_start_time_ticks":0,"state":"ready","worktree":{"key":key,"label":label}});
    let mut registry = json!({"format_version":2,"aliases":{},"leases":{a.clone():lease(&a,one,"one"),b.clone():lease(&b,two,"two")},"selected_servers":{},"last_synchronized_at_unix_ms":null,"pending_operations":[]});
    let path = fixture.registry();
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, registry.to_string()).unwrap();
    assert_eq!(
        first.call("nook_list", json!({}))["data"]["entries"][0]["resource_id"],
        a
    );
    assert_eq!(
        second.call("nook_list", json!({}))["data"]["entries"][0]["resource_id"],
        b
    );
    let mut all = fixture.spawn(&["--scope", "user"]);
    all.initialize("2025-11-25");
    let page = all.call("nook_list", json!({"limit":1}));
    assert_eq!(page["data"]["entries"].as_array().unwrap().len(), 1);
    let cursor = page["data"]["next_cursor"].clone();
    assert!(cursor.is_string());
    assert_eq!(
        all.call("nook_list", json!({"cursor":cursor,"limit":1}))["data"]["entries"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    registry["leases"].as_object_mut().unwrap().remove(&b);
    fs::write(&path, registry.to_string()).unwrap();
    assert_eq!(
        all.call("nook_list", json!({"cursor":cursor}))["error"]["code"],
        "STALE_CURSOR"
    );
    first.close();
    second.close();
    all.close();
}

#[test]
fn provider_aware_and_combined_legacy_registries_are_observation_only() {
    for version in [4, 5] {
        let fixture = Fixture::new();
        fixture.global("http://127.0.0.1:0");
        let owner = Uuid::new_v4();
        let mut registry = compose_registry::registry(&fixture.root, owner, false);
        registry["format_version"] = json!(version);
        registry["tunnels"] = json!({owner.to_string(): {
            "id":owner,"owner_id":owner,"hostname":"web.localhost",
            "target":"http://127.0.0.1:8080/",
            "supervisor":{"pid":1,"process_start_time_ticks":2},
            "child":null,"url":null,"metrics_port":1234,"phase":"failed","committed":true
        }});
        if version == 4 {
            registry.as_object_mut().unwrap().remove("compose_projects");
            registry["tunnels"][owner.to_string()]["provider"] = json!("ngrok");
            registry["tunnels"][owner.to_string()]["alias"] = json!(true);
        }
        let path = fixture.registry();
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let original = serde_json::to_vec_pretty(&registry).unwrap();
        fs::write(&path, &original).unwrap();
        let mut client = fixture.spawn(&[]);
        client.initialize("2025-11-25");
        let result = client.call("nook_list", json!({}));
        assert_eq!(result["ok"], true, "{result}");
        client.close();
        assert_eq!(fs::read(&path).unwrap(), original);
        assert!(!path.with_extension("pre-v6.json").exists());
        assert!(!path.with_extension("lock").exists());
    }
}
