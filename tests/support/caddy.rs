#![allow(dead_code)]

use std::fs::{self, File};
use std::io::{Read, Write};
use std::net::{Ipv4Addr, TcpListener, TcpStream};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

pub(crate) struct CaddyHarness {
    root: PathBuf,
    config: PathBuf,
    admin: AdminAddress,
    http_port: u16,
    https_port: u16,
    child: Child,
}

enum AdminAddress {
    Tcp(u16),
    Unix(PathBuf),
}

impl AdminAddress {
    fn caddy_address(&self) -> String {
        match self {
            Self::Tcp(port) => format!("127.0.0.1:{port}"),
            Self::Unix(socket) => format!("unix/{}|0660", socket.display()),
        }
    }

    fn client_address(&self) -> String {
        match self {
            Self::Tcp(port) => format!("http://127.0.0.1:{port}"),
            Self::Unix(socket) => format!("unix/{}", socket.display()),
        }
    }
}

impl CaddyHarness {
    pub(crate) fn start() -> Self {
        Self::start_with_ports(available_port(), available_port(), false, false)
    }

    pub(crate) fn start_on_standard_ports() -> Self {
        Self::start_with_ports(80, 443, true, true)
    }

    fn start_with_ports(
        http_port: u16,
        https_port: u16,
        trust_test_certificate: bool,
        unix_admin: bool,
    ) -> Self {
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
        if trust_test_certificate {
            generate_test_certificate(&root);
        }
        let admin = if unix_admin {
            AdminAddress::Unix(root.join("admin.socket"))
        } else {
            AdminAddress::Tcp(available_port())
        };
        let config = root.join("Caddyfile");
        fs::write(
            &config,
            format!(
                "{{\n\tadmin \"{}\"\n\tauto_https disable_redirects\n\tskip_install_trust\n}}\n\nhttp://localhost:{http_port} {{\n\trespond \"http-ok\"\n}}\n\nhttps://localhost:{https_port} {{\n\ttls internal\n\trespond \"https-ok\"\n}}\n",
                admin.caddy_address()
            ),
        )
        .unwrap();
        let stdout = File::create(root.join("stdout.log")).unwrap();
        let stderr = File::create(root.join("stderr.log")).unwrap();
        let mut command = isolated_command(&root);
        if trust_test_certificate {
            command.env("SSL_CERT_FILE", root.join("test-upstream.crt"));
        }
        let child = command
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
            admin,
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
        self.admin.client_address()
    }

    pub(crate) fn admin_socket(&self) -> Option<&Path> {
        match &self.admin {
            AdminAddress::Unix(socket) => Some(socket),
            AdminAddress::Tcp(_) => None,
        }
    }

    pub(crate) fn config_json(&self) -> String {
        let response = admin_get(&self.admin).expect("Caddy Admin API request failed");
        let body = response
            .windows(4)
            .position(|window| window == b"\r\n\r\n")
            .map(|index| &response[index + 4..])
            .expect("Caddy Admin API returned an invalid HTTP response");
        String::from_utf8(body.to_vec()).expect("Caddy configuration must be UTF-8 JSON")
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
                "{{\n\tadmin \"{}\"\n\tauto_https disable_redirects\n\tskip_install_trust\n}}\n\nhttp://localhost:{} {{\n{directives}\n}}\n\nhttps://localhost:{} {{\n\ttls internal\n\trespond \"https-ok\"\n}}\n",
                self.admin.caddy_address(), self.http_port, self.https_port
            ),
        )
        .unwrap();
        self.reload_current();
    }

    pub(crate) fn reload_sites(&self, sites: &str) {
        fs::write(
            &self.config,
            format!(
                "{{\n\tadmin \"{}\"\n\tauto_https disable_redirects\n\tskip_install_trust\n}}\n\n{sites}\n",
                self.admin.caddy_address()
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
            .arg(self.admin.caddy_address())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .unwrap();
        assert!(status.success(), "isolated Caddy reload failed");
    }

    fn wait_until_ready(&self) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if admin_responds(&self.admin) {
                return;
            }
            thread::sleep(Duration::from_millis(25));
        }
        let log = fs::read_to_string(self.root.join("stderr.log")).unwrap_or_default();
        panic!("isolated Caddy did not start:\n{log}");
    }
}

fn generate_test_certificate(root: &Path) {
    let status = Command::new("openssl")
        .args(["req", "-x509", "-newkey", "rsa:2048", "-nodes"])
        .arg("-keyout")
        .arg(root.join("test-upstream.key"))
        .arg("-out")
        .arg(root.join("test-upstream.crt"))
        .args([
            "-subj",
            "/CN=localhost",
            "-addext",
            "subjectAltName=DNS:localhost",
            "-days",
            "1",
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .expect("OpenSSL is required for integration tests");
    assert!(
        status.success(),
        "failed to generate trusted test certificate"
    );
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

fn admin_responds(admin: &AdminAddress) -> bool {
    admin_get(admin).is_some_and(|response| {
        response.starts_with(b"HTTP/1.1 200") || response.starts_with(b"HTTP/1.0 200")
    })
}

fn admin_get(admin: &AdminAddress) -> Option<Vec<u8>> {
    let (mut stream, host): (Box<dyn ReadWrite>, String) = match admin {
        AdminAddress::Tcp(port) => {
            let stream = TcpStream::connect((Ipv4Addr::LOCALHOST, *port)).ok()?;
            (Box::new(stream), format!("127.0.0.1:{port}"))
        }
        AdminAddress::Unix(socket) => {
            let stream = UnixStream::connect(socket).ok()?;
            (Box::new(stream), "localhost".into())
        }
    };
    write!(
        stream,
        "GET /config/ HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n"
    )
    .ok()?;
    let mut response = Vec::new();
    stream.read_to_end(&mut response).ok()?;
    Some(response)
}

trait ReadWrite: Read + Write {}
impl<T: Read + Write> ReadWrite for T {}

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
