mod support;

use std::fs::File;
use std::net::{Ipv4Addr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use support::caddy::CaddyHarness;

#[test]
fn https_upstreams_verify_trust_expiry_and_hostname() {
    let harness = CaddyHarness::start();
    let valid = Certificate::generate(harness.root(), "valid", "localhost", false);
    let untrusted = Certificate::generate(harness.root(), "untrusted", "localhost", false);
    let mismatch = Certificate::generate(harness.root(), "mismatch", "wrong.example", false);
    let expired = Certificate::generate(harness.root(), "expired", "localhost", true);
    let mut servers = vec![
        TlsUpstream::start(&valid),
        TlsUpstream::start(&untrusted),
        TlsUpstream::start(&mismatch),
        TlsUpstream::start(&expired),
    ];
    harness.reload_http_site(&format!(
        "\thandle /valid {{\n\t\treverse_proxy https://localhost:{} {{\n\t\t\ttransport http {{\n\t\t\t\ttls_trust_pool file {}\n\t\t\t}}\n\t\t}}\n\t}}\n\thandle /untrusted {{\n\t\treverse_proxy https://localhost:{}\n\t}}\n\thandle /mismatch {{\n\t\treverse_proxy https://localhost:{} {{\n\t\t\ttransport http {{\n\t\t\t\ttls_trust_pool file {}\n\t\t\t}}\n\t\t}}\n\t}}\n\thandle /expired {{\n\t\treverse_proxy https://localhost:{} {{\n\t\t\ttransport http {{\n\t\t\t\ttls_trust_pool file {}\n\t\t\t}}\n\t\t}}\n\t}}",
        servers[0].port,
        valid.cert.display(),
        servers[1].port,
        servers[2].port,
        mismatch.cert.display(),
        servers[3].port,
        expired.cert.display(),
    ));

    assert_eq!(
        request_status(&format!("{}/valid", harness.http_url())),
        200
    );
    for path in ["untrusted", "mismatch", "expired"] {
        assert_eq!(
            request_status(&format!("{}/{path}", harness.http_url())),
            502,
            "{path} TLS failure should be reported as a proxy error"
        );
    }
    drop(servers.drain(..));
}

fn request_status(url: &str) -> u16 {
    match ureq::get(url).call() {
        Ok(response) => response.status().as_u16(),
        Err(ureq::Error::StatusCode(code)) => code,
        Err(error) => panic!("request failed before receiving an HTTP response: {error}"),
    }
}

struct Certificate {
    cert: PathBuf,
    key: PathBuf,
}

impl Certificate {
    fn generate(root: &Path, name: &str, hostname: &str, expired: bool) -> Self {
        let cert = root.join(format!("{name}.crt"));
        let key = root.join(format!("{name}.key"));
        let mut command = Command::new("openssl");
        command.args(["req", "-x509", "-newkey", "rsa:2048", "-nodes"]);
        command.arg("-keyout").arg(&key).arg("-out").arg(&cert);
        command.args(["-subj", &format!("/CN={hostname}")]);
        command.args(["-addext", &format!("subjectAltName=DNS:{hostname}")]);
        if expired {
            command.args([
                "-not_before",
                "20000101000000Z",
                "-not_after",
                "20010101000000Z",
            ]);
        } else {
            command.args(["-days", "1"]);
        }
        let status = command
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .expect("OpenSSL is required for TLS integration tests");
        assert!(status.success(), "failed to generate {name} certificate");
        Self { cert, key }
    }
}

struct TlsUpstream {
    port: u16,
    child: Child,
}

impl TlsUpstream {
    fn start(certificate: &Certificate) -> Self {
        let port = available_port();
        let stdout = File::create(certificate.cert.with_extension("stdout.log")).unwrap();
        let stderr = File::create(certificate.cert.with_extension("stderr.log")).unwrap();
        let child = Command::new("openssl")
            .args(["s_server", "-quiet", "-www", "-accept"])
            .arg(format!("127.0.0.1:{port}"))
            .arg("-cert")
            .arg(&certificate.cert)
            .arg("-key")
            .arg(&certificate.key)
            .stdout(Stdio::from(stdout))
            .stderr(Stdio::from(stderr))
            .spawn()
            .expect("OpenSSL is required for TLS integration tests");
        let upstream = Self { port, child };
        upstream.wait_until_ready();
        upstream
    }

    fn wait_until_ready(&self) {
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline {
            if TcpStream::connect((Ipv4Addr::LOCALHOST, self.port)).is_ok() {
                return;
            }
            thread::sleep(Duration::from_millis(20));
        }
        panic!("TLS upstream on port {} did not start", self.port);
    }
}

impl Drop for TlsUpstream {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn available_port() -> u16 {
    TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}
