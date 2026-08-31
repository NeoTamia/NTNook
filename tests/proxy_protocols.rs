#![cfg(unix)]

mod support;

use std::io::{Read, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream, UdpSocket};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::Value;
use support::caddy::CaddyHarness;

#[test]
fn caddy_routes_preserve_proxy_protocols_and_reject_unmanaged_hosts() {
    let harness = CaddyHarness::start();
    let mut upstream = Upstream::start(harness.root());
    let unavailable = available_port();
    harness.reload_sites(&format!(
        ":{} {{
	@managed {{
		host run.localhost alias.localhost preserve.localhost
		remote_ip 127.0.0.0/8 ::1
	}}
	handle @managed {{
		reverse_proxy 127.0.0.1:{} {{
			header_up Host {{http.request.host}}
		}}
	}}
	@down host down.localhost
	handle @down {{
		reverse_proxy 127.0.0.1:{unavailable}
	}}
}}

https://localhost:{} {{
	tls internal
	reverse_proxy 127.0.0.1:{}
}}",
        harness.http_port(),
        upstream.port,
        harness.https_port(),
        upstream.port,
    ));

    for host in ["run.localhost", "alias.localhost"] {
        let response = raw_request(harness.http_port(), host, "/ok", &[]);
        assert!(response.starts_with("HTTP/1.1 200"));
        assert!(response.ends_with("ok"));
    }
    assert_eq!(
        ureq::get(format!("{}/ok", harness.https_url()))
            .config()
            .tls_config(
                ureq::tls::TlsConfig::builder()
                    .disable_verification(true)
                    .build()
            )
            .build()
            .call()
            .unwrap()
            .status(),
        200
    );

    let headers_response = raw_request(harness.http_port(), "preserve.localhost", "/headers", &[]);
    let headers = response_body(&headers_response);
    let headers: Value = serde_json::from_str(headers).unwrap();
    assert_eq!(headers["host"], "preserve.localhost");
    assert_eq!(headers["forwarded_host"], "preserve.localhost");
    assert_eq!(headers["forwarded_proto"], "http");
    assert!(
        headers["forwarded_for"]
            .as_str()
            .unwrap()
            .contains("127.0.0.1")
    );

    let websocket = raw_request(
        harness.http_port(),
        "run.localhost",
        "/websocket",
        &[
            ("Connection", "Upgrade"),
            ("Upgrade", "websocket"),
            ("Sec-WebSocket-Version", "13"),
            ("Sec-WebSocket-Key", "dGhlIHNhbXBsZSBub25jZQ=="),
        ],
    );
    assert!(websocket.starts_with("HTTP/1.1 101"));
    assert!(websocket.ends_with("upgraded"));

    let (sse, first_event, second_event) = timed_request(
        harness.http_port(),
        "run.localhost",
        "/sse",
        "data: first",
        "data: second",
    );
    assert!(sse.contains("Content-Type: text/event-stream"));
    assert!(sse.contains("data: first\n\ndata: second\n\n"));
    assert!(first_event < Duration::from_millis(200));
    assert!(second_event >= Duration::from_millis(200));
    let (stream, first_chunk, second_chunk) = timed_request(
        harness.http_port(),
        "run.localhost",
        "/stream",
        "first-",
        "second",
    );
    assert!(response_body(&stream).contains("first-"));
    assert!(response_body(&stream).contains("second"));
    assert!(first_chunk < Duration::from_millis(200));
    assert!(second_chunk >= Duration::from_millis(200));

    let http2 = Command::new("curl")
        .args(["--http2", "-sk", "-o", "/dev/null", "-w", "%{http_version}"])
        .arg(format!("{}/ok", harness.https_url()))
        .output()
        .expect("curl with HTTP/2 support is required for protocol integration tests");
    assert!(http2.status.success());
    assert_eq!(String::from_utf8(http2.stdout).unwrap(), "2");

    assert!(
        !response_body(&raw_request(
            harness.http_port(),
            "evil.example",
            "/ok",
            &[],
        ))
        .contains("ok")
    );
    assert!(
        !response_body(&raw_request_to(
            non_loopback_address(harness.http_port()),
            "run.localhost",
            "/ok",
            &[],
        ))
        .contains("ok")
    );
    assert!(
        raw_request(harness.http_port(), "down.localhost", "/", &[]).starts_with("HTTP/1.1 502")
    );
    upstream.stop();
}

fn raw_request(port: u16, host: &str, path: &str, headers: &[(&str, &str)]) -> String {
    raw_request_to(
        SocketAddr::from((Ipv4Addr::LOCALHOST, port)),
        host,
        path,
        headers,
    )
}

fn raw_request_to(address: SocketAddr, host: &str, path: &str, headers: &[(&str, &str)]) -> String {
    let mut stream = TcpStream::connect(address).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(1)))
        .unwrap();
    write!(stream, "GET {path} HTTP/1.1\r\nHost: {host}\r\n").unwrap();
    for (name, value) in headers {
        write!(stream, "{name}: {value}\r\n").unwrap();
    }
    write!(stream, "Connection: close\r\n\r\n").unwrap();
    let mut response = Vec::new();
    let mut buffer = [0_u8; 4096];
    loop {
        match stream.read(&mut buffer) {
            Ok(0) => break,
            Ok(read) => response.extend_from_slice(&buffer[..read]),
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) =>
            {
                break;
            }
            Err(error) => panic!("proxy response read failed: {error}"),
        }
    }
    String::from_utf8(response).unwrap()
}

fn non_loopback_address(port: u16) -> SocketAddr {
    let socket = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).unwrap();
    socket.connect("192.0.2.1:9").unwrap();
    SocketAddr::new(socket.local_addr().unwrap().ip(), port)
}

fn timed_request(
    port: u16,
    host: &str,
    path: &str,
    first_marker: &str,
    second_marker: &str,
) -> (String, Duration, Duration) {
    let mut stream = TcpStream::connect((Ipv4Addr::LOCALHOST, port)).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(1)))
        .unwrap();
    write!(
        stream,
        "GET {path} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n"
    )
    .unwrap();
    let start = Instant::now();
    let mut response = Vec::new();
    let mut buffer = [0_u8; 1024];
    let mut first_at = None;
    let mut second_at = None;
    loop {
        let read = stream.read(&mut buffer).unwrap();
        if read == 0 {
            break;
        }
        response.extend_from_slice(&buffer[..read]);
        let text = String::from_utf8_lossy(&response);
        if first_at.is_none() && text.contains(first_marker) {
            first_at = Some(start.elapsed());
        }
        if text.contains(second_marker) {
            second_at = Some(start.elapsed());
        }
    }
    (
        String::from_utf8(response).unwrap(),
        first_at.expect("first streamed chunk was not received"),
        second_at.expect("second streamed chunk was not received"),
    )
}

fn response_body(response: &str) -> &str {
    response.split_once("\r\n\r\n").unwrap().1
}

struct Upstream {
    port: u16,
    child: Option<Child>,
}

impl Upstream {
    fn start(root: &std::path::Path) -> Self {
        let port = available_port();
        let script = root.join("protocol-upstream.py");
        std::fs::write(&script, PYTHON_UPSTREAM).unwrap();
        let child = Command::new("/usr/bin/python3")
            .arg(&script)
            .arg(port.to_string())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let upstream = Self {
            port,
            child: Some(child),
        };
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline {
            if TcpStream::connect((Ipv4Addr::LOCALHOST, port)).is_ok() {
                return upstream;
            }
            thread::sleep(Duration::from_millis(20));
        }
        panic!("protocol upstream did not start");
    }

    fn stop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

impl Drop for Upstream {
    fn drop(&mut self) {
        self.stop();
    }
}

fn available_port() -> u16 {
    TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

const PYTHON_UPSTREAM: &str = r#"
import http.server, json, sys, time

class Handler(http.server.BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"
    def log_message(self, *_): pass
    def send(self, body, content_type="text/plain"):
        body = body.encode()
        self.send_response(200)
        self.send_header("Content-Type", content_type)
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)
    def do_GET(self):
        if self.path == "/websocket" and self.headers.get("Upgrade", "").lower() == "websocket":
            self.send_response(101, "Switching Protocols")
            self.send_header("Connection", "Upgrade")
            self.send_header("Upgrade", "websocket")
            self.end_headers()
            self.wfile.write(b"upgraded")
            self.wfile.flush()
        elif self.path == "/headers":
            self.send(json.dumps({
                "host": self.headers.get("Host"),
                "forwarded_host": self.headers.get("X-Forwarded-Host"),
                "forwarded_proto": self.headers.get("X-Forwarded-Proto"),
                "forwarded_for": self.headers.get("X-Forwarded-For"),
            }), "application/json")
        elif self.path == "/sse":
            first, second = b"data: first\n\n", b"data: second\n\n"
            self.send_response(200)
            self.send_header("Content-Type", "text/event-stream")
            self.send_header("Content-Length", str(len(first) + len(second)))
            self.end_headers()
            self.wfile.write(first)
            self.wfile.flush()
            time.sleep(.25)
            self.wfile.write(second)
            self.wfile.flush()
        elif self.path == "/stream":
            self.send_response(200)
            self.send_header("Content-Type", "application/octet-stream")
            self.send_header("Transfer-Encoding", "chunked")
            self.end_headers()
            self.wfile.write(b"6\r\nfirst-\r\n")
            self.wfile.flush()
            time.sleep(.25)
            self.wfile.write(b"6\r\nsecond\r\n0\r\n\r\n")
            self.wfile.flush()
        else:
            self.send("ok")

http.server.ThreadingHTTPServer(("127.0.0.1", int(sys.argv[1])), Handler).serve_forever()
"#;
