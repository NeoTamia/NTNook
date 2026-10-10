//! Loopback smoke for the hostname a real Vite dev server will accept.
//!
//! Skips when `node` is not on PATH, or when `vite@6` is not already in the
//! npm cache. The lookup is `npm exec --offline`, so the suite never installs
//! Vite. The dev server is `node` running Vite's CLI directly, so the test
//! can stop that process group. Nook's detector is not involved and nothing
//! reads `package.json`.

#![cfg(unix)]

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

#[test]
fn vite_accepts_the_nook_hostname_on_loopback() {
    if Command::new("node").arg("--version").output().is_err() {
        return;
    }
    if Command::new("npm").arg("--version").output().is_err() {
        return;
    }

    let directory = std::env::temp_dir().join(format!("nook-vite-smoke-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&directory).unwrap();
    std::fs::write(
        directory.join("index.html"),
        "<!doctype html><p>nook-smoke-ok</p>\n",
    )
    .unwrap();
    let Some(vite_js) = locate_vite_cli() else {
        return;
    };
    let listener = TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0))).unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    let port_arg = port.to_string();
    let hostname = "nook-smoke.localhost";
    let log_path = directory.join("vite.log");
    let log_file = std::fs::File::create(&log_path).unwrap();

    let child = Command::new("node")
        .arg(&vite_js)
        .args(["--host", "127.0.0.1", "--port", &port_arg, "--strictPort"])
        .current_dir(&directory)
        .env("__VITE_ADDITIONAL_SERVER_ALLOWED_HOSTS", hostname)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::from(log_file))
        .process_group(0)
        .spawn()
        .expect("node vite");
    let mut session = ViteSession {
        pid: child.id(),
        child,
        directory,
        log_path,
        stopped: false,
    };

    if !wait_for_port(port, Duration::from_secs(30)) {
        let log = session.stop();
        panic!("vite did not accept loopback connections: {log}");
    }

    let allowed = http_get(port, hostname);
    let blocked = http_get(port, "blocked.example");
    let _ = session.stop();

    assert!(
        allowed.status == 200 && allowed.body.contains("nook-smoke-ok"),
        "nook hostname should be served, got {} {}",
        allowed.status,
        allowed.body
    );
    assert!(
        blocked.status == 403 || blocked.body.contains("Blocked request"),
        "foreign host should be rejected, got {} {}",
        blocked.status,
        blocked.body
    );
}

fn locate_vite_cli() -> Option<PathBuf> {
    let output = Command::new("npm")
        .args([
            "exec",
            "--offline",
            "--yes",
            "--package=vite@6",
            "--",
            "node",
            "-e",
            "const fs=require('node:fs');const path=require('node:path');const bin=process.env.PATH.split(path.delimiter).map(dir=>path.join(dir,'vite')).find(candidate=>fs.existsSync(candidate));if(!bin) process.exit(1);process.stdout.write(fs.realpathSync(bin));",
        ])
        .stdin(Stdio::null())
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let path = String::from_utf8(output.stdout).ok()?;
    if path.is_empty() {
        return None;
    }
    Some(PathBuf::from(path))
}

struct ViteSession {
    pid: u32,
    child: Child,
    directory: PathBuf,
    log_path: PathBuf,
    stopped: bool,
}

impl ViteSession {
    fn stop(&mut self) -> String {
        if self.stopped {
            return String::new();
        }
        self.stopped = true;
        let _ = Command::new("kill")
            .args(["-TERM", &format!("-{}", self.pid)])
            .status();
        let _ = self.child.kill();
        let _ = self.child.wait();
        let log = std::fs::read_to_string(&self.log_path).unwrap_or_default();
        let _ = std::fs::remove_dir_all(&self.directory);
        log
    }
}

impl Drop for ViteSession {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}

fn wait_for_port(port: u16, budget: Duration) -> bool {
    let started = Instant::now();
    while started.elapsed() < budget {
        if TcpStream::connect_timeout(
            &SocketAddr::from(([127, 0, 0, 1], port)),
            Duration::from_millis(200),
        )
        .is_ok()
        {
            return true;
        }
        thread::sleep(Duration::from_millis(100));
    }
    false
}

struct HttpResponse {
    status: u16,
    body: String,
}

fn http_get(port: u16, host: &str) -> HttpResponse {
    let started = Instant::now();
    let mut last = String::from("no response");
    while started.elapsed() < Duration::from_secs(10) {
        match request(port, host) {
            Ok(response) if response.status != 0 => return response,
            Ok(response) => last = response.body,
            Err(error) => last = error,
        }
        thread::sleep(Duration::from_millis(100));
    }
    HttpResponse {
        status: 0,
        body: last,
    }
}

fn request(port: u16, host: &str) -> Result<HttpResponse, String> {
    let mut stream = TcpStream::connect(SocketAddr::from(([127, 0, 0, 1], port)))
        .map_err(|error| error.to_string())?;
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .map_err(|error| error.to_string())?;
    write!(
        stream,
        "GET / HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n"
    )
    .map_err(|error| error.to_string())?;
    let mut buffer = Vec::new();
    stream
        .read_to_end(&mut buffer)
        .map_err(|error| error.to_string())?;
    let text = String::from_utf8_lossy(&buffer).into_owned();
    let status = text
        .split_whitespace()
        .nth(1)
        .and_then(|value| value.parse().ok())
        .unwrap_or(0);
    Ok(HttpResponse { status, body: text })
}
