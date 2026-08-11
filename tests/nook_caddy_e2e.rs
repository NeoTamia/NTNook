mod support;

use std::fs;
use std::io::{Read, Write};
use std::net::{Ipv4Addr, TcpListener, TcpStream};
use std::path::Path;
use std::process::{Command, Output, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use support::caddy::CaddyHarness;

#[test]
fn real_nook_routes_alias_and_run_through_caddy_https() {
    if std::env::var_os("NOOK_E2E_DIRECT").is_none() && std::env::var_os("NOOK_E2E_NETNS").is_none()
    {
        let output = Command::new("unshare")
            .args(["--user", "--map-root-user", "--net"])
            .arg(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "real_nook_routes_alias_and_run_through_caddy_https",
                "--nocapture",
                "--test-threads=1",
            ])
            .env("NOOK_E2E_NETNS", "1")
            .output()
            .expect("unshare is required for isolated standard-port integration tests");
        assert!(
            output.status.success(),
            "network namespace test failed:\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    if std::env::var_os("NOOK_E2E_NETNS").is_some() {
        assert_success(
            &Command::new("ip")
                .args(["link", "set", "lo", "up"])
                .output()
                .unwrap(),
        );
    }
    let harness = CaddyHarness::start_on_standard_ports();
    let directory = harness.root().join("nook-e2e");
    let config_home = directory.join("config");
    let state_home = directory.join("state");
    fs::create_dir_all(config_home.join("nook")).unwrap();
    fs::write(
        config_home.join("nook/config.toml"),
        format!(
            "format_version = 1\ncaddy_admin = \"{}\"\n",
            harness.admin_url()
        ),
    )
    .unwrap();
    let unique = format!("nooke2e{}", std::process::id());
    let alias_host = format!("alias-{unique}.localhost");
    let run_host = format!("run-{unique}.localhost");
    let http_alias_host = format!("http-alias-{unique}.localhost");
    let tls_alias_host = format!("tls-alias-{unique}.localhost");
    let http_run_host = format!("http-run-{unique}.localhost");
    let foreign_host = format!("foreign-{unique}.localhost");
    harness.reload_sites(&format!(
        "http://localhost:80 {{
	respond \"http-ok\"
}}

https://localhost:443 {{
	tls internal
	@foreign host {foreign_host}
	handle @foreign {{
		respond \"foreign\"
	}}
	respond \"https-ok\"
}}"
    ));
    let status = nook(&config_home, &state_home, &["status"]);
    assert_success(&status);
    assert!(String::from_utf8_lossy(&status.stdout).contains("caddy\tok"));
    assert!(
        String::from_utf8_lossy(&status.stderr)
            .contains(&format!("caddy trust --address {}", harness.admin_url()))
    );
    let foreign_config = fetch_config(&harness);
    let rejected = nook(
        &config_home,
        &state_home,
        &["alias", "set", &foreign_host, "39999", "--force"],
    );
    assert!(!rejected.status.success());
    assert!(String::from_utf8_lossy(&rejected.stderr).contains("foreign Caddy route"));
    assert_eq!(fetch_config(&harness), foreign_config);

    let concurrent_hosts = [
        format!("one-{unique}.localhost"),
        format!("two-{unique}.localhost"),
    ];
    let creations: Vec<_> = concurrent_hosts
        .iter()
        .enumerate()
        .map(|(index, hostname)| {
            let config_home = config_home.clone();
            let state_home = state_home.clone();
            let hostname = hostname.clone();
            thread::spawn(move || {
                nook(
                    &config_home,
                    &state_home,
                    &["alias", "set", &hostname, &format!("31{}01", index)],
                )
            })
        })
        .collect();
    for creation in creations {
        assert_success(&creation.join().unwrap());
    }
    let removals: Vec<_> = concurrent_hosts
        .iter()
        .map(|hostname| {
            let config_home = config_home.clone();
            let state_home = state_home.clone();
            let hostname = hostname.clone();
            thread::spawn(move || nook(&config_home, &state_home, &["alias", "remove", &hostname]))
        })
        .collect();
    for removal in removals {
        assert_success(&removal.join().unwrap());
    }

    let upstream = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let upstream_port = upstream.local_addr().unwrap().port();
    let upstream_server = thread::spawn(move || {
        for _ in 0..2 {
            let (mut stream, _) = upstream.accept().unwrap();
            let mut request = [0_u8; 2048];
            let _ = stream.read(&mut request).unwrap();
            stream
                .write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Length: 14\r\nConnection: close\r\n\r\nalias-via-nook",
                )
                .unwrap();
        }
    });
    let set_alias = nook(
        &config_home,
        &state_home,
        &["alias", "set", &alias_host, &upstream_port.to_string()],
    );
    assert!(
        set_alias.status.success(),
        "stderr: {}\nCaddy log:\n{}",
        String::from_utf8_lossy(&set_alias.stderr),
        fs::read_to_string(harness.root().join("stderr.log")).unwrap_or_default()
    );
    assert_eq!(https_body(&alias_host), "alias-via-nook");
    harness.reload();
    let list = nook(&config_home, &state_home, &["list"]);
    assert_success(&list);
    assert!(
        String::from_utf8(list.stdout)
            .unwrap()
            .contains(&alias_host)
    );
    assert_eq!(https_body(&alias_host), "alias-via-nook");
    upstream_server.join().unwrap();
    assert_success(&nook(
        &config_home,
        &state_home,
        &["alias", "remove", &alias_host],
    ));

    let http_upstream = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let http_upstream_port = http_upstream.local_addr().unwrap().port();
    let http_upstream_server = thread::spawn(move || {
        let (mut stream, _) = http_upstream.accept().unwrap();
        let mut request = [0_u8; 2048];
        let _ = stream.read(&mut request).unwrap();
        stream
            .write_all(
                b"HTTP/1.1 200 OK\r\nContent-Length: 14\r\nConnection: close\r\n\r\nhttp-via-nook!",
            )
            .unwrap();
    });
    let http_target = format!("http://127.0.0.1:{http_upstream_port}");
    assert_success(&nook(
        &config_home,
        &state_home,
        &["alias", "set", &http_alias_host, &http_target, "--no-tls"],
    ));
    assert_eq!(http_body(&http_alias_host), "http-via-nook!");
    http_upstream_server.join().unwrap();
    assert_not_exposed_over_https(&http_alias_host, "http-via-nook!");
    assert_success(&nook(
        &config_home,
        &state_home,
        &["alias", "remove", &http_alias_host],
    ));

    let tls_reservation = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let tls_upstream_port = tls_reservation.local_addr().unwrap().port();
    drop(tls_reservation);
    let mut tls_upstream = Command::new("openssl")
        .args(["s_server", "-quiet", "-www", "-accept"])
        .arg(format!("127.0.0.1:{tls_upstream_port}"))
        .arg("-cert")
        .arg(harness.root().join("test-upstream.crt"))
        .arg("-key")
        .arg(harness.root().join("test-upstream.key"))
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    wait_for_port(tls_upstream_port);
    let tls_target = format!("https://localhost:{tls_upstream_port}");
    assert_success(&nook(
        &config_home,
        &state_home,
        &["alias", "set", &tls_alias_host, &tls_target],
    ));
    assert!(https_body(&tls_alias_host).contains("s_server"));
    assert_success(&nook(
        &config_home,
        &state_home,
        &["alias", "remove", &tls_alias_host],
    ));
    let _ = tls_upstream.kill();
    let _ = tls_upstream.wait();

    let script = "import os,http.server;http.server.ThreadingHTTPServer(('127.0.0.1',int(os.environ['PORT'])),http.server.SimpleHTTPRequestHandler).serve_forever()";
    let mut running = Command::new(env!("CARGO_BIN_EXE_nook"))
        .args([
            "run",
            "--name",
            &run_host,
            "--",
            "/usr/bin/python3",
            "-c",
            script,
        ])
        .env("XDG_CONFIG_HOME", &config_home)
        .env("XDG_STATE_HOME", &state_home)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    wait_for_state(&state_home, &run_host, "ready");
    assert!(https_body(&run_host).contains("Directory listing"));
    assert_success(
        &Command::new("kill")
            .args(["-INT", &running.id().to_string()])
            .output()
            .unwrap(),
    );
    assert_eq!(running.wait().unwrap().code(), Some(130));

    let mut http_running = Command::new(env!("CARGO_BIN_EXE_nook"))
        .args([
            "run",
            "--name",
            &http_run_host,
            "--no-tls",
            "--",
            "/usr/bin/python3",
            "-c",
            script,
        ])
        .env("XDG_CONFIG_HOME", &config_home)
        .env("XDG_STATE_HOME", &state_home)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    wait_for_state(&state_home, &http_run_host, "ready");
    assert!(http_body(&http_run_host).contains("Directory listing"));
    assert_not_exposed_over_https(&http_run_host, "Directory listing");
    assert_success(
        &Command::new("kill")
            .args(["-TERM", &http_running.id().to_string()])
            .output()
            .unwrap(),
    );
    assert_eq!(http_running.wait().unwrap().code(), Some(143));
    assert!(!fetch_config(&harness).contains(&alias_host));
    assert!(!fetch_config(&harness).contains(&run_host));
    assert!(!fetch_config(&harness).contains(&http_alias_host));
    assert!(!fetch_config(&harness).contains(&tls_alias_host));
    assert!(!fetch_config(&harness).contains(&http_run_host));
}

fn nook(config_home: &Path, state_home: &Path, arguments: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_nook"))
        .args(arguments)
        .env("XDG_CONFIG_HOME", config_home)
        .env("XDG_STATE_HOME", state_home)
        .output()
        .unwrap()
}

fn https_body(hostname: &str) -> String {
    let output = curl("https", 443, hostname);
    assert_success(&output);
    String::from_utf8(output.stdout).unwrap()
}

fn http_body(hostname: &str) -> String {
    let output = curl("http", 80, hostname);
    assert_success(&output);
    String::from_utf8(output.stdout).unwrap()
}

fn assert_not_exposed_over_https(hostname: &str, forbidden_body: &str) {
    let output = curl("https", 443, hostname);
    assert!(
        !output.status.success()
            || !String::from_utf8_lossy(&output.stdout).contains(forbidden_body),
        "HTTP-only route unexpectedly exposed over HTTPS"
    );
}

fn curl(scheme: &str, port: u16, hostname: &str) -> Output {
    Command::new("curl")
        .args([
            "--fail",
            "--silent",
            "--show-error",
            "--insecure",
            "--resolve",
        ])
        .arg(format!("{hostname}:{port}:127.0.0.1"))
        .arg(format!("{scheme}://{hostname}/"))
        .output()
        .unwrap()
}

fn fetch_config(harness: &CaddyHarness) -> String {
    harness.config_json()
}

fn wait_for_state(state_home: &Path, hostname: &str, expected: &str) {
    let state_path = state_home.join("nook/state.json");
    let deadline = Instant::now() + Duration::from_secs(3);
    while Instant::now() < deadline {
        if fs::read_to_string(&state_path).is_ok_and(|state| {
            state.contains(hostname) && state.contains(&format!("\"state\": \"{expected}\""))
        }) {
            return;
        }
        thread::sleep(Duration::from_millis(20));
    }
    panic!("{hostname} did not become {expected}");
}

fn wait_for_port(port: u16) {
    let deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() < deadline {
        if TcpStream::connect((Ipv4Addr::LOCALHOST, port)).is_ok() {
            return;
        }
        thread::sleep(Duration::from_millis(20));
    }
    panic!("TLS upstream did not listen on {port}");
}

fn assert_success(output: &Output) {
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}
