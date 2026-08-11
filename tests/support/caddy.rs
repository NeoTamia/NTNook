#![allow(dead_code)]

use std::fs::{self, File};
use std::io::{Read, Write};
use std::net::{Ipv4Addr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

pub(crate) struct CaddyHarness {
    root: PathBuf,
    config: PathBuf,
    admin_port: u16,
    http_port: u16,
    https_port: u16,
    child: Child,
}

impl CaddyHarness {
    pub(crate) fn start() -> Self {
        assert_supported_version();
        let root = std::env::temp_dir().join(format!(
            "nook-caddy-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&root).unwrap();
        let admin_port = available_port();
        let http_port = available_port();
        let https_port = available_port();
        let config = root.join("Caddyfile");
        fs::write(
            &config,
            format!(
                "{{\n\tadmin 127.0.0.1:{admin_port}\n\tauto_https disable_redirects\n\tskip_install_trust\n}}\n\nhttp://localhost:{http_port} {{\n\trespond \"http-ok\"\n}}\n\nhttps://localhost:{https_port} {{\n\ttls internal\n\trespond \"https-ok\"\n}}\n"
            ),
        )
        .unwrap();
        let stdout = File::create(root.join("stdout.log")).unwrap();
        let stderr = File::create(root.join("stderr.log")).unwrap();
        let child = isolated_command(&root)
            .args(["run", "--config"])
            .arg(&config)
            .args(["--adapter", "caddyfile"])
            .stdout(Stdio::from(stdout))
            .stderr(Stdio::from(stderr))
            .spawn()
            .expect("Caddy 2.11.x must be installed for integration tests");
        let harness = Self {
            root,
            config,
            admin_port,
            http_port,
            https_port,
            child,
        };
        harness.wait_until_ready();
        harness
    }

    pub(crate) fn root(&self) -> &Path {
        &self.root
    }

    pub(crate) fn admin_url(&self) -> String {
        format!("http://127.0.0.1:{}", self.admin_port)
    }

    pub(crate) fn http_url(&self) -> String {
        format!("http://localhost:{}", self.http_port)
    }

    pub(crate) fn https_url(&self) -> String {
        format!("https://localhost:{}", self.https_port)
    }

    pub(crate) fn http_port(&self) -> u16 {
        self.http_port
    }

    pub(crate) fn https_port(&self) -> u16 {
        self.https_port
    }

    pub(crate) fn reload(&self) {
        self.reload_current();
    }

    pub(crate) fn reload_http_site(&self, directives: &str) {
        fs::write(
            &self.config,
            format!(
                "{{\n\tadmin 127.0.0.1:{}\n\tauto_https disable_redirects\n\tskip_install_trust\n}}\n\nhttp://localhost:{} {{\n{directives}\n}}\n\nhttps://localhost:{} {{\n\ttls internal\n\trespond \"https-ok\"\n}}\n",
                self.admin_port, self.http_port, self.https_port
            ),
        )
        .unwrap();
        self.reload_current();
    }

    pub(crate) fn reload_sites(&self, sites: &str) {
        fs::write(
            &self.config,
            format!(
                "{{\n\tadmin 127.0.0.1:{}\n\tauto_https disable_redirects\n\tskip_install_trust\n}}\n\n{sites}\n",
                self.admin_port
            ),
        )
        .unwrap();
        self.reload_current();
    }

    fn reload_current(&self) {
        let status = isolated_command(&self.root)
            .args(["reload", "--config"])
            .arg(&self.config)
            .args(["--adapter", "caddyfile", "--address"])
            .arg(format!("127.0.0.1:{}", self.admin_port))
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .unwrap();
        assert!(status.success(), "isolated Caddy reload failed");
    }

    fn wait_until_ready(&self) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if admin_responds(self.admin_port) {
                return;
            }
            thread::sleep(Duration::from_millis(25));
        }
        let log = fs::read_to_string(self.root.join("stderr.log")).unwrap_or_default();
        panic!("isolated Caddy did not start:\n{log}");
    }
}

impl Drop for CaddyHarness {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn isolated_command(root: &Path) -> Command {
    let mut command = Command::new("caddy");
    command
        .env("HOME", root.join("home"))
        .env("XDG_DATA_HOME", root.join("data"))
        .env("XDG_CONFIG_HOME", root.join("config"));
    command
}

fn available_port() -> u16 {
    TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

fn admin_responds(port: u16) -> bool {
    let Ok(mut stream) = TcpStream::connect((Ipv4Addr::LOCALHOST, port)) else {
        return false;
    };
    let _ = write!(
        stream,
        "GET /config/ HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n"
    );
    let mut response = [0_u8; 32];
    stream.read(&mut response).is_ok_and(|size| {
        response[..size].starts_with(b"HTTP/1.1 200")
            || response[..size].starts_with(b"HTTP/1.0 200")
    })
}

fn assert_supported_version() {
    let output = Command::new("caddy")
        .arg("version")
        .output()
        .expect("Caddy 2.11.x must be installed for integration tests");
    assert!(output.status.success());
    let version = String::from_utf8_lossy(&output.stdout);
    assert!(
        version.trim_start().starts_with("v2.11."),
        "integration harness requires Caddy 2.11.x, found {version}"
    );
}
