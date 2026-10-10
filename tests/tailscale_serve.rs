//! Tailscale Serve lifecycle scenarios against the real `nook` binary.
//!
//! This target runs without the libtest harness because the same executable
//! plays three roles: the scenario runner, a stateful fake `tailscale` CLI
//! (selected through `NOOK_FAKE_TAILSCALE_STATE`), and the application child
//! started by `nook run` (selected by the `__child` argument). The fake keeps
//! one Serve configuration in a JSON file guarded by a file lock, so several
//! concurrent `nook` processes observe the same tailnet node. A loopback stub
//! answers the Caddy Admin API requests Nook makes for its local routes.

use std::env;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::net::{Ipv4Addr, TcpListener, TcpStream};
use std::panic::{self, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use uuid::Uuid;

const STATE_VARIABLE: &str = "NOOK_FAKE_TAILSCALE_STATE";
const DNS_NAME: &str = "nook-test.example.ts.net";

fn main() {
    let arguments: Vec<String> = env::args().collect();
    if arguments.get(1).map(String::as_str) == Some("__child") {
        std::process::exit(application_child(&arguments[2..]));
    }
    if let Some(state) = env::var_os(STATE_VARIABLE) {
        std::process::exit(fake_tailscale(Path::new(&state), &arguments[1..]));
    }
    let filter = arguments
        .iter()
        .skip(1)
        .find(|argument| !argument.starts_with('-'))
        .cloned();
    let scenarios: [(&str, fn()); 10] = [
        (
            "missing_client_fails_before_spawn_without_changes",
            missing_client_fails_before_spawn_without_changes,
        ),
        (
            "unready_tailnet_is_diagnosed_before_any_change",
            unready_tailnet_is_diagnosed_before_any_change,
        ),
        (
            "run_takes_port_443_exports_its_url_and_cleans_up_preserving_exit_code",
            run_takes_port_443_exports_its_url_and_cleans_up_preserving_exit_code,
        ),
        (
            "project_configuration_opts_in_and_no_tailscale_opts_out",
            project_configuration_opts_in_and_no_tailscale_opts_out,
        ),
        (
            "locally_shadowed_ports_are_skipped_and_explained",
            locally_shadowed_ports_are_skipped_and_explained,
        ),
        (
            "foreign_registrations_and_funnel_are_skipped_and_never_modified",
            foreign_registrations_and_funnel_are_skipped_and_never_modified,
        ),
        (
            "concurrent_runs_receive_distinct_ports",
            concurrent_runs_receive_distinct_ports,
        ),
        (
            "alias_registration_persists_until_alias_remove",
            alias_registration_persists_until_alias_remove,
        ),
        (
            "crashed_supervisor_registration_is_removed_by_prune",
            crashed_supervisor_registration_is_removed_by_prune,
        ),
        (
            "tailscale_down_up_defers_then_restores_without_taking_foreign_ports",
            tailscale_down_up_defers_then_restores_without_taking_foreign_ports,
        ),
    ];
    let mut failed = Vec::new();
    let mut ran = 0;
    for (name, scenario) in scenarios {
        if filter
            .as_deref()
            .is_some_and(|filter| !name.contains(filter))
        {
            continue;
        }
        ran += 1;
        print!("test {name} ... ");
        let _ = std::io::stdout().flush();
        match panic::catch_unwind(AssertUnwindSafe(scenario)) {
            Ok(()) => println!("ok"),
            Err(_) => {
                println!("FAILED");
                failed.push(name);
            }
        }
    }
    println!(
        "\ntest result: {}. {} passed; {} failed",
        if failed.is_empty() { "ok" } else { "FAILED" },
        ran - failed.len(),
        failed.len()
    );
    if !failed.is_empty() {
        println!("failures: {failed:?}");
        std::process::exit(101);
    }
}

fn missing_client_fails_before_spawn_without_changes() {
    let world = World::new("missing");
    let missing = world.root.join(if cfg!(windows) {
        "missing-tailscale.exe"
    } else {
        "missing-tailscale"
    });
    let marker = world.root.join("child.json");
    let run = world
        .nook_command(&["run", "--tailscale", "--name", "web", "--"])
        .args(world.child_arguments(&marker, 0, None))
        .env("NOOK_TAILSCALE", &missing)
        .env_remove(STATE_VARIABLE)
        .output()
        .unwrap();
    assert_eq!(run.status.code(), Some(1), "{}", describe(&run));
    assert!(stderr(&run).contains("was not found"), "{}", describe(&run));
    assert!(stderr(&run).contains("https://tailscale.com/download"));
    thread::sleep(Duration::from_millis(200));
    assert!(!marker.exists());
    assert!(world.caddy_routes().is_empty());
    assert!(
        world.registry_value()["leases"]
            .as_object()
            .is_none_or(|leases| leases.is_empty())
    );
}

fn unready_tailnet_is_diagnosed_before_any_change() {
    let world = World::new("unready");
    let marker = world.root.join("child.json");
    let attempt_run = || {
        world
            .nook_command(&["run", "--tailscale", "--name", "web", "--"])
            .args(world.child_arguments(&marker, 0, None))
            .output()
            .unwrap()
    };

    world.update_tailscale(|state| {
        state["backend_state"] = json!("NeedsLogin");
        state["auth_url"] = json!("https://login.tailscale.com/a/nook-test-login");
    });
    let run = attempt_run();
    assert_eq!(run.status.code(), Some(1), "{}", describe(&run));
    assert!(stderr(&run).contains("https://login.tailscale.com/a/nook-test-login"));
    let alias = world.nook(&["alias", "set", "api", "3000", "--tailscale"]);
    assert_eq!(alias.status.code(), Some(1));
    assert!(stderr(&alias).contains("https://login.tailscale.com/a/nook-test-login"));

    world.update_tailscale(|state| {
        state["backend_state"] = json!("Running");
        state["https"] = json!(false);
    });
    let run = attempt_run();
    assert_eq!(run.status.code(), Some(1));
    assert!(stderr(&run).contains("HTTPS certificates are not enabled"));
    assert!(stderr(&run).contains("https://login.tailscale.com/admin/dns"));

    world.update_tailscale(|state| state["daemon"] = json!(false));
    let run = attempt_run();
    assert_eq!(run.status.code(), Some(1));
    assert!(stderr(&run).contains("tailscaled is not reachable"));

    world.update_tailscale(|state| state["version"] = json!("1.50.1"));
    let run = attempt_run();
    assert_eq!(run.status.code(), Some(1));
    assert!(stderr(&run).contains("1.52 or newer"));

    world.update_tailscale(|state| {
        state["version"] = json!("1.80.2");
        state["daemon"] = json!(true);
        state["https"] = json!(true);
    });
    let external = world.nook(&["alias", "set", "ext", "https://example.com", "--tailscale"]);
    assert_eq!(external.status.code(), Some(1));
    assert!(stderr(&external).contains("loopback upstream"));

    thread::sleep(Duration::from_millis(200));
    assert!(!marker.exists(), "the child must never start");
    assert!(world.caddy_routes().is_empty());
    assert!(world.nook(&["alias", "list"]).stdout.is_empty());
    assert!(world.serve_calls().is_empty(), "{:?}", world.serve_calls());
    assert_eq!(world.serve_config(), json!({}));
}

fn run_takes_port_443_exports_its_url_and_cleans_up_preserving_exit_code() {
    let world = World::new("run");
    let marker = world.root.join("child.json");
    let run = world
        .nook_command(&["run", "--tailscale", "--name", "web", "--"])
        .args(world.child_arguments(&marker, 7, None))
        .output()
        .unwrap();
    assert_eq!(run.status.code(), Some(7), "{}", describe(&run));
    let observed = read_json(&marker);
    assert_eq!(observed["url"], format!("https://{DNS_NAME}"));
    let port = observed["port"].as_u64().unwrap();
    assert_eq!(
        observed["serve"]["Web"][format!("{DNS_NAME}:443")]["Handlers"]["/"]["Proxy"],
        format!("http://127.0.0.1:{port}"),
        "Serve must proxy straight to the loopback application"
    );
    assert_eq!(observed["serve"]["TCP"]["443"]["HTTPS"], true);
    let line = stderr(&run)
        .lines()
        .find(|line| line.starts_with("nook: "))
        .map(str::to_owned)
        .expect("run information line");
    assert!(line.contains("url=https://web.localhost"), "{line}");
    assert!(
        line.contains(&format!("tailscale_url=https://{DNS_NAME}")),
        "{line}"
    );

    assert_eq!(world.serve_config(), json!({}));
    assert!(
        world.registry_value()["tailscale"]["registrations"]
            .as_object()
            .unwrap()
            .is_empty()
    );
    assert!(world.caddy_routes_are_empty());

    let local = world
        .nook_command(&["run", "--name", "plain", "--"])
        .args(world.child_arguments(&marker, 0, None))
        .output()
        .unwrap();
    assert!(local.status.success(), "{}", describe(&local));
    assert_eq!(read_json(&marker)["url"], Value::Null);
}

fn project_configuration_opts_in_and_no_tailscale_opts_out() {
    let world = World::new("project");
    let config = world.root.join("nook.toml");
    fs::write(
        &config,
        "format_version = 1\nname = \"site\"\ntailscale = true\n",
    )
    .unwrap();
    let marker = world.root.join("child.json");
    let run = |flags: &[&str]| {
        let config = config.display().to_string();
        let mut arguments = vec!["run", "--config", &config];
        arguments.extend_from_slice(flags);
        arguments.push("--");
        let output = world
            .nook_command(&arguments)
            .args(world.child_arguments(&marker, 0, None))
            .output()
            .unwrap();
        assert!(output.status.success(), "{}", describe(&output));
        read_json(&marker)["url"].clone()
    };
    assert_eq!(run(&[]), format!("https://{DNS_NAME}"));
    assert_eq!(run(&["--no-tailscale"]), Value::Null);

    fs::write(
        &config,
        "format_version = 1\nname = \"site\"\ntailscale = false\n",
    )
    .unwrap();
    assert_eq!(run(&[]), Value::Null);
    assert_eq!(run(&["--tailscale"]), format!("https://{DNS_NAME}"));
    assert_eq!(world.serve_config(), json!({}));
}

fn locally_shadowed_ports_are_skipped_and_explained() {
    let world = World::new("shadowed");
    let foreign = json!({
        "TCP": {"443": {"HTTPS": true}},
        "Web": {format!("{DNS_NAME}:443"): {"Handlers": {"/": {"Proxy": "http://127.0.0.1:9999"}}}}
    });
    world.update_tailscale(|state| {
        state["serve"] = foreign.clone();
        state["tailscale_ips"] = json!(["127.0.0.1"]);
    });
    // Bind failure means another program already holds the port, which Nook
    // must skip just the same.
    let _listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 8443));
    let marker = world.root.join("child.json");
    let run = world
        .nook_command(&["run", "--tailscale", "--name", "web", "--"])
        .args(world.child_arguments(&marker, 0, None))
        .output()
        .unwrap();
    assert!(run.status.success(), "{}", describe(&run));
    assert_eq!(
        read_json(&marker)["url"],
        format!("https://{DNS_NAME}:8444")
    );
    let explanation = stderr(&run)
        .lines()
        .find(|line| line.contains("skipped Tailscale Serve port 8443"))
        .map(str::to_owned)
        .unwrap_or_else(|| panic!("no skip explanation: {}", describe(&run)));
    assert!(
        explanation.contains("127.0.0.1:8443"),
        "the explanation names the listener: {explanation}"
    );

    world.update_tailscale(|state| state["tailscale_ips"] = json!(["127.0.0.2"]));
    let run = world
        .nook_command(&["run", "--tailscale", "--name", "docs", "--"])
        .args(world.child_arguments(&marker, 0, None))
        .output()
        .unwrap();
    assert!(run.status.success(), "{}", describe(&run));
    assert_eq!(
        read_json(&marker)["url"],
        format!("https://{DNS_NAME}:8443"),
        "a listener on another local address does not receive tailnet connections"
    );
    assert!(!stderr(&run).contains("skipped Tailscale Serve port"));
    assert_eq!(world.serve_config(), foreign);
}

fn foreign_registrations_and_funnel_are_skipped_and_never_modified() {
    let world = World::new("foreign");
    let foreign = json!({
        "TCP": {"443": {"HTTPS": true}, "5432": {"TCPForward": "127.0.0.1:5432"}},
        "Web": {format!("{DNS_NAME}:443"): {"Handlers": {"/": {"Proxy": "http://127.0.0.1:9999"}}}},
        "AllowFunnel": {format!("{DNS_NAME}:443"): true},
        "Foreground": {"session-1": {"TCP": {"8443": {"HTTPS": true}}, "Web": {format!("{DNS_NAME}:8443"): {"Handlers": {"/": {"Proxy": "http://127.0.0.1:9998"}}}}}}
    });
    world.update_tailscale(|state| state["serve"] = foreign.clone());
    let marker = world.root.join("child.json");
    let run = world
        .nook_command(&["run", "--tailscale", "--name", "web", "--"])
        .args(world.child_arguments(&marker, 0, None))
        .output()
        .unwrap();
    assert!(run.status.success(), "{}", describe(&run));
    let observed = read_json(&marker);
    assert_eq!(observed["url"], format!("https://{DNS_NAME}:8444"));
    assert_ne!(
        observed["serve"]["AllowFunnel"][format!("{DNS_NAME}:8444")],
        true
    );

    let alias = world.nook(&["alias", "set", "api", "3000", "--tailscale"]);
    assert!(alias.status.success(), "{}", describe(&alias));
    assert!(stdout(&alias).contains(&format!(
        "https://{DNS_NAME}:8444 -> http://127.0.0.1:3000/"
    )));
    assert!(world.nook(&["alias", "remove", "api"]).status.success());

    assert_eq!(world.serve_config(), foreign);
    assert_only_private_serve_commands(&world);
}

fn concurrent_runs_receive_distinct_ports() {
    let world = World::new("concurrent");
    let release = world.root.join("release");
    let runs: Vec<(PathBuf, Child)> = ["one", "two", "three"]
        .into_iter()
        .map(|name| {
            let marker = world.root.join(format!("{name}.json"));
            let child = world
                .nook_command(&["run", "--tailscale", "--name", name, "--"])
                .args(world.child_arguments(&marker, 0, Some(&release)))
                .stdout(Stdio::null())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap();
            (marker, child)
        })
        .collect();
    let mut urls: Vec<String> = runs
        .iter()
        .map(|(marker, _)| {
            wait_for(Duration::from_secs(30), || marker.exists());
            read_json(marker)["url"].as_str().unwrap().to_owned()
        })
        .collect();
    urls.sort();
    assert_eq!(
        urls,
        [
            format!("https://{DNS_NAME}"),
            format!("https://{DNS_NAME}:8443"),
            format!("https://{DNS_NAME}:8444"),
        ]
    );
    let registrations = world.registry_value()["tailscale"]["registrations"].clone();
    assert_eq!(registrations.as_object().unwrap().len(), 3);
    File::create(&release).unwrap();
    for (_, child) in runs {
        let output = wait_with_timeout(child, Duration::from_secs(30));
        assert!(output.status.success(), "{}", describe(&output));
    }
    assert_eq!(world.serve_config(), json!({}));
    assert!(world.caddy_routes_are_empty());
}

fn alias_registration_persists_until_alias_remove() {
    let world = World::new("alias");
    let set = world.nook(&["alias", "api", "3000", "--tailscale"]);
    assert!(set.status.success(), "{}", describe(&set));
    assert_eq!(
        stdout(&set),
        format!(
            "api.localhost -> http://127.0.0.1:3000/\nhttps://{DNS_NAME} -> http://127.0.0.1:3000/\n"
        )
    );
    for _ in 0..2 {
        let list = world.nook(&["list"]);
        assert!(list.status.success(), "{}", describe(&list));
        assert_eq!(
            stdout(&list),
            format!(
                "alias\tpersistent\tapi.localhost\thttp://127.0.0.1:3000/\thttps://api.localhost\thttps://{DNS_NAME}\n"
            )
        );
    }
    let status = world.nook(&["tailscale", "status"]);
    assert!(status.status.success(), "{}", describe(&status));
    let status = stdout(&status);
    assert!(status.contains("client\t1.80.2\n"), "{status}");
    assert!(status.contains("tailscaled\treachable\n"));
    assert!(status.contains("backend\tRunning\n"));
    assert!(status.contains("https\tenabled\n"));
    assert!(status.contains(&format!("dns_name\t{DNS_NAME}\n")));
    assert!(status.contains(&format!(
        "registration\t443\tapi.localhost\thttp://127.0.0.1:3000\thttps://{DNS_NAME}\tactive\n"
    )));
    assert_eq!(
        world.serve_config()["Web"][format!("{DNS_NAME}:443")]["Handlers"]["/"]["Proxy"],
        "http://127.0.0.1:3000"
    );

    let replaced = world.nook(&["alias", "set", "api", "3001", "--force"]);
    assert!(replaced.status.success(), "{}", describe(&replaced));
    assert_eq!(
        world.serve_config(),
        json!({}),
        "a replacement without --tailscale withdraws the old registration"
    );
    let exposed = world.nook(&["alias", "set", "api", "3002", "--force", "--tailscale"]);
    assert!(exposed.status.success(), "{}", describe(&exposed));
    let exposed_serve = world.serve_config();

    world.update_tailscale(|state| {
        state["serve_error"] = json!("Access denied: serve config denied")
    });
    let refused = world.nook(&["alias", "set", "api", "3003", "--force", "--tailscale"]);
    assert_eq!(refused.status.code(), Some(1), "{}", describe(&refused));
    assert!(stderr(&refused).contains("--operator=$USER"));
    assert_eq!(
        stdout(&world.nook(&["alias", "list"])),
        "api.localhost -> http://127.0.0.1:3002/\n",
        "a failed forced replacement keeps the previous alias"
    );
    let routes = serde_json::to_string(&world.caddy_routes()).unwrap();
    assert!(routes.contains("127.0.0.1:3002"), "{routes}");
    assert!(!routes.contains("127.0.0.1:3003"), "{routes}");
    assert_eq!(world.serve_config(), exposed_serve);
    world.update_tailscale(|state| state["serve_error"] = Value::Null);

    let remove = world.nook(&["alias", "remove", "api"]);
    assert!(remove.status.success(), "{}", describe(&remove));
    assert_eq!(world.serve_config(), json!({}));
    assert!(world.caddy_routes_are_empty());
    assert_only_private_serve_commands(&world);
}

fn crashed_supervisor_registration_is_removed_by_prune() {
    let world = World::new("crash");
    world.crash_exposed_run("crashed");

    let prune = world.nook(&["prune"]);
    assert!(prune.status.success(), "{}", describe(&prune));
    assert!(
        stdout(&prune).contains("removed_dead=1 "),
        "{}",
        describe(&prune)
    );
    assert!(
        stdout(&prune).contains("tailscale_restored=0 tailscale_removed=1\n"),
        "{}",
        describe(&prune)
    );
    assert_eq!(world.serve_config(), json!({}));

    world.crash_exposed_run("forgotten");
    world.update_tailscale(|state| state["serve"] = json!({}));
    let prune = world.nook(&["prune"]);
    assert!(
        stdout(&prune).contains("tailscale_restored=0 tailscale_removed=1\n"),
        "a registration Tailscale already dropped is still counted: {}",
        describe(&prune)
    );

    world.crash_exposed_run("crashed-again");

    // `tailscale status` skips the Caddy reconciliation, so the crashed lease
    // is still recorded and only its process liveness proves the owner gone.
    wait_for(Duration::from_secs(10), || {
        world.nook(&["tailscale", "status"]).status.success() && world.serve_config() == json!({})
    });
    assert!(
        world.registry_value()["leases"]
            .as_object()
            .is_some_and(|leases| leases.len() == 1)
    );
    let prune = world.nook(&["prune"]);
    assert!(prune.status.success(), "{}", describe(&prune));
    assert!(
        stdout(&prune).contains("removed_dead=1 "),
        "{}",
        describe(&prune)
    );
    assert!(
        stdout(&prune).contains("tailscale_removed=0\n"),
        "{}",
        describe(&prune)
    );
    assert!(
        world.registry_value()["tailscale"]["registrations"]
            .as_object()
            .unwrap()
            .is_empty()
    );
}

fn tailscale_down_up_defers_then_restores_without_taking_foreign_ports() {
    let world = World::new("down-up");
    assert!(
        world
            .nook(&["alias", "api", "3000", "--tailscale"])
            .status
            .success()
    );
    let registered = world.serve_config();

    world.update_tailscale(|state| state["serve"] = json!({}));
    let prune = world.nook(&["prune"]);
    assert!(
        stdout(&prune).contains("tailscale_restored=1 tailscale_removed=0\n"),
        "{}",
        describe(&prune)
    );
    assert_eq!(world.serve_config(), registered);

    world.update_tailscale(|state| {
        state["backend_state"] = json!("Stopped");
        state["serve"] = json!({});
    });
    let list = world.nook(&["list"]);
    assert!(list.status.success(), "{}", describe(&list));
    assert!(stderr(&list).contains("Tailscale Serve registration(s) are pending"));
    assert!(stdout(&list).contains(&format!("https://{DNS_NAME}")));
    let status = world.nook(&["tailscale", "status"]);
    assert_eq!(status.status.code(), Some(1));
    assert!(stdout(&status).contains("backend\tStopped\n"));
    assert!(stdout(&status).contains("\tpending\n"));
    assert!(stderr(&status).contains("run `tailscale up` yourself"));

    world.update_tailscale(|state| state["backend_state"] = json!("Running"));
    assert!(world.nook(&["list"]).status.success());
    assert_eq!(
        world.serve_config(),
        registered,
        "the alias keeps its port and URL"
    );

    let foreign = json!({
        "TCP": {"443": {"HTTPS": true}},
        "Web": {format!("{DNS_NAME}:443"): {"Handlers": {"/": {"Proxy": "http://127.0.0.1:9999"}}}}
    });
    world.update_tailscale(|state| state["serve"] = foreign.clone());
    let list = world.nook(&["list"]);
    assert!(list.status.success());
    assert!(
        stderr(&list).contains("Nook does not own"),
        "{}",
        describe(&list)
    );
    let status = world.nook(&["tailscale", "status"]);
    assert!(
        stdout(&status).contains("\tforeign\n"),
        "{}",
        describe(&status)
    );
    let remove = world.nook(&["alias", "remove", "api"]);
    assert!(remove.status.success());
    assert_eq!(world.serve_config(), foreign);
    assert!(
        world.registry_value()["tailscale"]["registrations"]
            .as_object()
            .unwrap()
            .is_empty()
    );
    assert_only_private_serve_commands(&world);
}

fn assert_only_private_serve_commands(world: &World) {
    for call in world.tailscale_calls() {
        let call: Vec<&str> = call.iter().map(String::as_str).collect();
        let allowed = match call.as_slice() {
            ["version"] | ["status", "--json"] | ["serve", "status", "--json"] => true,
            ["serve", "--bg", port, target] => {
                port.starts_with("--https=")
                    && (target.starts_with("http://127.0.0.1:")
                        || target.starts_with("http://localhost:")
                        || target.starts_with("https://127.0.0.1:")
                        || target.starts_with("https://localhost:"))
            }
            ["serve", port, "off"] => port.starts_with("--https="),
            _ => false,
        };
        assert!(allowed, "Nook ran a forbidden Tailscale command: {call:?}");
    }
}

struct World {
    root: PathBuf,
    config_home: PathBuf,
    state_home: PathBuf,
    tailscale_state: PathBuf,
    caddy: Arc<Mutex<Vec<Value>>>,
}

impl World {
    fn new(label: &str) -> Self {
        let root = env::temp_dir().join(format!("nook-tailscale-{label}-{}", Uuid::new_v4()));
        let config_home = root.join("config");
        let state_home = root.join("state");
        fs::create_dir_all(config_home.join("nook")).unwrap();
        fs::create_dir_all(&state_home).unwrap();
        let tailscale_state = root.join("tailscale.json");
        write_json(
            &tailscale_state,
            &json!({
                "version": "1.80.2",
                "daemon": true,
                "backend_state": "Running",
                "auth_url": "",
                "dns_name": format!("{DNS_NAME}."),
                "https": true,
                "serve": {},
                "calls": []
            }),
        );
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        fs::write(
            config_home.join("nook/config.toml"),
            format!(
                "format_version = 1\ncaddy_admin = \"http://{}\"\n",
                listener.local_addr().unwrap()
            ),
        )
        .unwrap();
        let caddy = Arc::new(Mutex::new(Vec::new()));
        let routes = Arc::clone(&caddy);
        thread::spawn(move || serve_caddy(listener, routes));
        Self {
            root,
            config_home,
            state_home,
            tailscale_state,
            caddy,
        }
    }

    fn nook_command(&self, arguments: &[&str]) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_nook"));
        command
            .args(arguments)
            .env("NOOK_DISABLE_UPDATE_CHECK", "1")
            .env("XDG_CONFIG_HOME", &self.config_home)
            .env("XDG_STATE_HOME", &self.state_home)
            .env("NOOK_TAILSCALE", env::current_exe().unwrap())
            .env(STATE_VARIABLE, &self.tailscale_state)
            .stdin(Stdio::null());
        command
    }

    fn nook(&self, arguments: &[&str]) -> Output {
        let child = self
            .nook_command(arguments)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        wait_with_timeout(child, Duration::from_secs(60))
    }

    fn child_arguments(&self, marker: &Path, code: i32, release: Option<&Path>) -> Vec<String> {
        vec![
            env::current_exe().unwrap().display().to_string(),
            "__child".into(),
            marker.display().to_string(),
            code.to_string(),
            release.map_or_else(|| "-".into(), |path| path.display().to_string()),
        ]
    }

    /// Starts an exposed run and kills its supervisor once the application
    /// accepts connections, leaving the lease and Serve registration behind.
    fn crash_exposed_run(&self, name: &str) {
        let marker = self.root.join(format!("{name}.json"));
        let release = self.root.join("never-released");
        let mut supervisor = self
            .nook_command(&["run", "--tailscale", "--name", name, "--"])
            .args(self.child_arguments(&marker, 0, Some(&release)))
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        wait_for(Duration::from_secs(30), || marker.exists());
        let port = read_json(&marker)["port"].as_u64().unwrap();
        wait_for(Duration::from_secs(10), || {
            TcpStream::connect((Ipv4Addr::LOCALHOST, port as u16)).is_ok()
        });
        let hostname = format!("{name}.localhost");
        wait_for(Duration::from_secs(10), || {
            self.registry_value()["leases"]
                .as_object()
                .is_some_and(|leases| {
                    leases
                        .values()
                        .any(|lease| lease["hostname"] == hostname.as_str())
                })
        });
        supervisor.kill().unwrap();
        supervisor.wait().unwrap();
        wait_for(Duration::from_secs(10), || {
            TcpStream::connect((Ipv4Addr::LOCALHOST, port as u16)).is_err()
        });
        assert_ne!(self.serve_config(), json!({}));
    }

    fn update_tailscale(&self, change: impl FnOnce(&mut Value)) {
        with_locked_state(&self.tailscale_state, change);
    }

    fn serve_config(&self) -> Value {
        let mut serve = Value::Null;
        with_locked_state(&self.tailscale_state, |state| {
            serve = state["serve"].clone()
        });
        serve
    }

    fn tailscale_calls(&self) -> Vec<Vec<String>> {
        let mut calls = Value::Null;
        with_locked_state(&self.tailscale_state, |state| {
            calls = state["calls"].clone()
        });
        serde_json::from_value(calls).unwrap()
    }

    fn serve_calls(&self) -> Vec<Vec<String>> {
        self.tailscale_calls()
            .into_iter()
            .filter(|call| {
                call.first().map(String::as_str) == Some("serve")
                    && call.get(1).map(String::as_str) != Some("status")
            })
            .collect()
    }

    fn registry_value(&self) -> Value {
        fs::read(self.state_home.join("nook/state.json"))
            .map(|bytes| serde_json::from_slice(&bytes).unwrap())
            .unwrap_or(Value::Null)
    }

    fn caddy_routes(&self) -> Vec<Value> {
        self.caddy.lock().unwrap().clone()
    }

    /// Nook keeps its (empty) managed container; no host route may remain.
    fn caddy_routes_are_empty(&self) -> bool {
        self.caddy_routes().iter().all(|route| {
            route
                .pointer("/handle/0/routes")
                .and_then(Value::as_array)
                .is_none_or(Vec::is_empty)
        })
    }
}

impl Drop for World {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn fake_tailscale(state_path: &Path, arguments: &[String]) -> i32 {
    let mut code = 0;
    with_locked_state(state_path, |state| {
        state["calls"]
            .as_array_mut()
            .unwrap()
            .push(json!(arguments));
        code = fake_command(state, arguments);
    });
    code
}

fn fake_command(state: &mut Value, arguments: &[String]) -> i32 {
    let arguments: Vec<&str> = arguments.iter().map(String::as_str).collect();
    let dns_name = state["dns_name"]
        .as_str()
        .unwrap()
        .trim_end_matches('.')
        .to_owned();
    if arguments == ["version"] {
        println!(
            "{}\n  tailscale commit: fake\n  go version: go1.24",
            state["version"].as_str().unwrap()
        );
        return 0;
    }
    if state["daemon"] != true {
        eprintln!(
            "failed to connect to local tailscaled; it doesn't appear to be running (sudo systemctl start tailscaled ?)"
        );
        return 1;
    }
    let running = state["backend_state"] == "Running";
    match arguments.as_slice() {
        ["status", "--json"] => {
            let cert_domains = if state["https"] == true {
                json!([dns_name])
            } else {
                Value::Null
            };
            println!(
                "{}",
                json!({
                    "Version": state["version"],
                    "BackendState": state["backend_state"],
                    "AuthURL": state["auth_url"],
                    "Self": {
                        "DNSName": state["dns_name"],
                        "TailscaleIPs": state["tailscale_ips"],
                        "Online": running
                    },
                    "CertDomains": cert_domains,
                    "Health": []
                })
            );
            0
        }
        ["serve", "status", "--json"] => {
            println!("{}", state["serve"]);
            0
        }
        ["serve", "--bg", https, target] if https.starts_with("--https=") => {
            if let Some(message) = state["serve_error"].as_str() {
                eprintln!("{message}");
                return 1;
            }
            if !running {
                eprintln!("error: Tailscale is not running");
                return 1;
            }
            if state["https"] != true {
                println!("Serve is not enabled on your tailnet.");
                return 1;
            }
            let port = &https["--https=".len()..];
            let serve = state["serve"].as_object_mut().unwrap();
            let tcp = serve.entry("TCP").or_insert_with(|| json!({}));
            if tcp.get(port).is_some_and(|entry| entry["HTTPS"] != true) {
                eprintln!("error: port {port} is already serving TCP");
                return 1;
            }
            tcp[port] = json!({"HTTPS": true});
            let web = serve.entry("Web").or_insert_with(|| json!({}));
            web[format!("{dns_name}:{port}")] =
                json!({"Handlers": {"/": {"Proxy": target.trim_end_matches('/')}}});
            0
        }
        ["serve", https, "off"] if https.starts_with("--https=") => {
            let port = &https["--https=".len()..];
            let host = format!("{dns_name}:{port}");
            let serve = state["serve"].as_object_mut().unwrap();
            let removed = serve
                .get_mut("Web")
                .and_then(|web| web.get_mut(&host))
                .and_then(|entry| entry["Handlers"].as_object_mut())
                .and_then(|handlers| handlers.remove("/"))
                .is_some();
            if !removed {
                eprintln!("error: failed to remove web serve: handler does not exist");
                return 1;
            }
            let handlers_left = serve["Web"][&host]["Handlers"]
                .as_object()
                .is_some_and(|handlers| !handlers.is_empty());
            if !handlers_left {
                serve["Web"].as_object_mut().unwrap().remove(&host);
                serve["TCP"].as_object_mut().unwrap().remove(port);
            }
            for key in ["Web", "TCP"] {
                if serve
                    .get(key)
                    .and_then(Value::as_object)
                    .is_some_and(|map| map.is_empty())
                {
                    serve.remove(key);
                }
            }
            0
        }
        _ => {
            eprintln!("fake tailscale: unsupported command {arguments:?}");
            2
        }
    }
}

fn application_child(arguments: &[String]) -> i32 {
    let marker = PathBuf::from(&arguments[0]);
    let code: i32 = arguments[1].parse().unwrap();
    let release = &arguments[2];
    let port: u16 = env::var("PORT").unwrap().parse().unwrap();
    let host = env::var("HOST").unwrap();
    let listener = TcpListener::bind((host.as_str(), port)).ok();
    let serve = env::var_os(STATE_VARIABLE).map_or(Value::Null, |path| {
        let mut serve = Value::Null;
        with_locked_state(Path::new(&path), |state| serve = state["serve"].clone());
        serve
    });
    write_json(
        &marker,
        &json!({
            "url": env::var("NOOK_TAILSCALE_URL").ok(),
            "port": port,
            "serve": serve
        }),
    );
    if release != "-" {
        let deadline = Instant::now() + Duration::from_secs(60);
        while !Path::new(release).exists() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(20));
        }
    }
    drop(listener);
    code
}

fn with_locked_state(path: &Path, change: impl FnOnce(&mut Value)) {
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(path.with_extension("lock"))
        .unwrap();
    lock.lock().unwrap();
    let mut state = read_json(path);
    change(&mut state);
    write_json(path, &state);
}

fn read_json(path: &Path) -> Value {
    serde_json::from_slice(&fs::read(path).unwrap()).unwrap()
}

fn write_json(path: &Path, value: &Value) {
    let temporary = path.with_extension(format!("{}.tmp", Uuid::new_v4()));
    fs::write(&temporary, serde_json::to_vec_pretty(value).unwrap()).unwrap();
    fs::rename(&temporary, path).unwrap();
}

fn wait_for(timeout: Duration, mut condition: impl FnMut() -> bool) {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if condition() {
            return;
        }
        thread::sleep(Duration::from_millis(50));
    }
    panic!("condition not reached within {timeout:?}");
}

fn wait_with_timeout(mut child: Child, timeout: Duration) -> Output {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if child.try_wait().unwrap().is_some() {
            return child.wait_with_output().unwrap();
        }
        thread::sleep(Duration::from_millis(20));
    }
    let _ = child.kill();
    let output = child.wait_with_output().unwrap();
    panic!("nook timed out: {}", describe(&output));
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn describe(output: &Output) -> String {
    format!(
        "status: {:?}\nstdout:\n{}\nstderr:\n{}",
        output.status,
        stdout(output),
        stderr(output)
    )
}

fn serve_caddy(listener: TcpListener, routes: Arc<Mutex<Vec<Value>>>) {
    for stream in listener.incoming() {
        let Ok(mut stream) = stream else { continue };
        let Some(request) = read_request(&mut stream) else {
            continue;
        };
        let header_end = request
            .windows(4)
            .position(|bytes| bytes == b"\r\n\r\n")
            .unwrap()
            + 4;
        let head = String::from_utf8_lossy(&request[..header_end]).into_owned();
        let first_line = head.lines().next().unwrap_or_default();
        if first_line.starts_with("GET /config/ ") {
            let current = routes.lock().unwrap().clone();
            respond(
                &mut stream,
                "200 OK",
                &json!({"apps":{"http":{"servers":{"https":{"listen":[":443"],"routes":current}}}}}),
                None,
            );
        } else if first_line.starts_with("GET /config/apps/http/servers/https/routes ") {
            let current = json!(*routes.lock().unwrap());
            respond(&mut stream, "200 OK", &current, Some("\"v1\""));
        } else if first_line.starts_with("PATCH /config/apps/http/servers/https/routes ") {
            *routes.lock().unwrap() = serde_json::from_slice(&request[header_end..]).unwrap();
            respond(&mut stream, "200 OK", &json!({}), None);
        } else {
            respond(
                &mut stream,
                "404 Not Found",
                &json!({"error": first_line}),
                None,
            );
        }
    }
}

fn read_request(stream: &mut TcpStream) -> Option<Vec<u8>> {
    let mut request = Vec::new();
    let mut buffer = [0_u8; 4096];
    let mut expected = None;
    loop {
        let read = stream.read(&mut buffer).ok()?;
        if read == 0 {
            return None;
        }
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
            return Some(request);
        }
    }
}

fn respond(stream: &mut TcpStream, status: &str, value: &Value, etag: Option<&str>) {
    let body = serde_json::to_vec(value).unwrap();
    let etag = etag.map_or(String::new(), |value| format!("ETag: {value}\r\n"));
    let _ = write!(
        stream,
        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\n{etag}Content-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    let _ = stream.write_all(&body);
}
