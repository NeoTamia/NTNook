use std::fs;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::process::{Command, Output};
use std::sync::{Arc, Mutex};
use std::thread;

use serde_json::{Value, json};

#[test]
fn alias_shortcuts_persist_list_and_remove_idempotently() {
    let directory = std::env::temp_dir().join(format!(
        "nook-cli-alias-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let config_home = directory.join("config");
    let state_home = directory.join("state");
    fs::create_dir_all(config_home.join("nook")).unwrap();

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let admin = format!("http://{}", listener.local_addr().unwrap());
    fs::write(
        config_home.join("nook/config.toml"),
        format!("format_version = 1\ncaddy_admin = \"{admin}\"\n"),
    )
    .unwrap();

    let routes = Arc::new(Mutex::new(Vec::<Value>::new()));
    let server_routes = Arc::clone(&routes);
    let server = thread::spawn(move || serve_caddy(listener, server_routes, 6));

    let set = nook(&config_home, &state_home, &["alias", "api", "3000"]);
    assert_success(&set);
    assert_eq!(
        String::from_utf8(set.stdout).unwrap(),
        "api.localhost -> http://127.0.0.1:3000/\n"
    );

    let list = nook(&config_home, &state_home, &["alias", "list"]);
    assert_success(&list);
    assert_eq!(
        String::from_utf8(list.stdout).unwrap(),
        "api.localhost -> http://127.0.0.1:3000/\n"
    );

    let remove = nook(&config_home, &state_home, &["alias", "--remove", "api"]);
    assert_success(&remove);
    assert_eq!(
        String::from_utf8(remove.stdout).unwrap(),
        "removed api.localhost\n"
    );

    let repeated = nook(&config_home, &state_home, &["alias", "remove", "api"]);
    assert_success(&repeated);
    assert_eq!(
        String::from_utf8(repeated.stdout).unwrap(),
        "alias api.localhost is not configured\n"
    );

    server.join().unwrap();
    assert!(routes.lock().unwrap().is_empty());
    fs::remove_dir_all(directory).unwrap();
}

fn nook(config_home: &Path, state_home: &Path, arguments: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_nook"))
        .args(arguments)
        .env("XDG_CONFIG_HOME", config_home)
        .env("XDG_STATE_HOME", state_home)
        .output()
        .unwrap()
}

fn assert_success(output: &Output) {
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn serve_caddy(listener: TcpListener, routes: Arc<Mutex<Vec<Value>>>, requests: usize) {
    for _ in 0..requests {
        let (mut stream, _) = listener.accept().unwrap();
        let request = read_request(&mut stream);
        let header_end = request
            .windows(4)
            .position(|bytes| bytes == b"\r\n\r\n")
            .unwrap()
            + 4;
        let head = String::from_utf8_lossy(&request[..header_end]);
        let first_line = head.lines().next().unwrap();
        if first_line.starts_with("GET /config/ ") {
            respond_json(
                &mut stream,
                &json!({"apps":{"http":{"servers":{"https":{"listen":[":443"],"routes":[]}}}}}),
                None,
            );
        } else if first_line.starts_with("GET /config/apps/http/servers/https/routes ") {
            respond_json(&mut stream, &json!(*routes.lock().unwrap()), Some("\"v1\""));
        } else if first_line.starts_with("PUT /config/apps/http/servers/https/routes ") {
            *routes.lock().unwrap() = serde_json::from_slice(&request[header_end..]).unwrap();
            respond_json(&mut stream, &json!({}), None);
        } else {
            panic!("unexpected request: {first_line}");
        }
    }
}

fn read_request(stream: &mut TcpStream) -> Vec<u8> {
    let mut request = Vec::new();
    let mut buffer = [0_u8; 4096];
    let mut expected = None;
    loop {
        let read = stream.read(&mut buffer).unwrap();
        assert!(read > 0);
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
            return request;
        }
    }
}

fn respond_json(stream: &mut TcpStream, value: &Value, etag: Option<&str>) {
    let body = serde_json::to_vec(value).unwrap();
    let etag = etag.map_or(String::new(), |value| format!("ETag: {value}\r\n"));
    write!(
        stream,
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n{etag}Content-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    )
    .unwrap();
    stream.write_all(&body).unwrap();
}
