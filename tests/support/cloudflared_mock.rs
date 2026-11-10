// Standalone native executable compiled by the Windows integration test.
use std::io::{Read, Write};
use std::net::TcpListener;
fn main() {
    let args: Vec<_> = std::env::args().collect();
    if args.iter().any(|a| a == "--version") {
        println!("cloudflared version 2026.10.7");
        return;
    }
    if args.iter().any(|a| a == "--help") {
        println!(
            "--metrics --pidfile --url --origin-server-name --origin-ca-pool --http-host-header --protocol"
        );
        return;
    }
    assert!(std::env::var_os("TUNNEL_TOKEN").is_none());
    let address = &args[args.iter().position(|a| a == "--metrics").unwrap() + 1];
    let listener = TcpListener::bind(address).unwrap();
    let pidfile = &args[args.iter().position(|a| a == "--pidfile").unwrap() + 1];
    std::fs::write(pidfile, std::process::id().to_string()).unwrap();
    for connection in listener.incoming() {
        let mut stream = connection.unwrap();
        stream
            .set_read_timeout(Some(std::time::Duration::from_secs(1)))
            .unwrap();
        let mut buffer = [0; 4096];
        let count = stream.read(&mut buffer).unwrap_or(0);
        let request = String::from_utf8_lossy(&buffer[..count]);
        let body = if request.contains("/quicktunnel ") {
            "{\"hostname\":\"windows-fixture.trycloudflare.com\"}"
        } else {
            "{}"
        };
        let _ = write!(
            stream,
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        );
    }
}
