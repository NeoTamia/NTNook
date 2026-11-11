// Standalone native ngrok fixture for the Windows process/argument test.
use std::{io::{Read, Write}, net::TcpListener};
fn main() {
    let args: Vec<_> = std::env::args().collect();
    if args.iter().any(|a| a == "--help") { println!("--name --url --config --traffic-policy-file --upstream-tls-verify --pooling-enabled"); return; }
    if args.get(1).map(String::as_str) == Some("config") { return; }
    assert!(std::env::var_os("NGROK_AUTHTOKEN").is_some());
    assert!(std::env::var_os("NGROK_API_KEY").is_none());
    let config = &args[args.iter().position(|a|a=="--config").unwrap()+1];
    let overlay = config.rsplit(',').next().unwrap();
    let text = std::fs::read_to_string(overlay).unwrap();
    let address = text.split("web_addr: ").nth(1).unwrap().split_whitespace().next().unwrap();
    let name = &args[args.iter().position(|a|a=="--name").unwrap()+1];
    let listener = TcpListener::bind(address).unwrap();
    for connection in listener.incoming() {
        let mut stream = connection.unwrap();
        stream.set_read_timeout(Some(std::time::Duration::from_secs(1))).unwrap();
        let _ = stream.read(&mut [0;4096]);
        let body = format!(r#"{{"endpoints":[{{"name":"{}","url":"https://windows-fixture.ngrok-free.dev","upstream":{{"url":"{}"}}}}]}}"#, name,args[2]);
        let _ = write!(stream,"HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",body.len(),body);
    }
}
