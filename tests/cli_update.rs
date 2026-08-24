use std::fs;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use uuid::Uuid;

const ARCHIVE_NAME: &str = "nook-x86_64-unknown-linux-musl.tar.xz";
const CHECKSUM_NAME: &str = "nook-x86_64-unknown-linux-musl.tar.xz.sha256";

struct ReleaseServer {
    tag: String,
    archive: Vec<u8>,
    checksum: String,
    latest_status: u16,
    hang: bool,
    hits: usize,
}

#[test]
fn update_check_reports_current_and_outdated_versions() {
    let home = test_home("check");
    let current = format!("v{}", env!("CARGO_PKG_VERSION"));
    let state = Arc::new(Mutex::new(ReleaseServer {
        tag: current.clone(),
        archive: Vec::new(),
        checksum: String::new(),
        latest_status: 200,
        hang: false,
        hits: 0,
    }));
    let url = spawn_server(Arc::clone(&state));

    let current_check = nook(
        &home,
        &["update", "--check"],
        &[("NOOK_UPDATE_RELEASES_URL", &url)],
    );
    assert_eq!(
        current_check.status.code(),
        Some(0),
        "{}",
        stderr(&current_check)
    );
    assert!(stdout(&current_check).contains("already the latest version"));

    state.lock().unwrap().tag = "v99.0.0".into();
    let outdated = nook(
        &home,
        &["update", "--check"],
        &[("NOOK_UPDATE_RELEASES_URL", &url)],
    );
    assert_eq!(outdated.status.code(), Some(1), "{}", stderr(&outdated));
    assert!(stdout(&outdated).contains("nook 99.0.0 is available"));
    fs::remove_dir_all(home).unwrap();
}

#[test]
fn update_check_network_failure_uses_exit_code_two() {
    let home = test_home("check-fail");
    let state = Arc::new(Mutex::new(ReleaseServer {
        tag: "v99.0.0".into(),
        archive: Vec::new(),
        checksum: String::new(),
        latest_status: 500,
        hang: false,
        hits: 0,
    }));
    let url = spawn_server(Arc::clone(&state));
    let output = nook(
        &home,
        &["update", "--check"],
        &[("NOOK_UPDATE_RELEASES_URL", &url)],
    );
    assert_eq!(output.status.code(), Some(2), "{}", stderr(&output));
    assert!(stderr(&output).contains("cannot fetch Nook releases"));
    fs::remove_dir_all(home).unwrap();
}

#[test]
fn warning_uses_fresh_cache_without_network() {
    let home = test_home("cached-warning");
    write_cache(&home, unix_ms_now(), "99.0.0");
    let output = nook(&home, &["config", "path"], &[]);
    assert!(output.status.success(), "{}", stderr(&output));
    assert!(
        stderr(&output)
            .contains("warning: nook 99.0.0 is available (installed 0.3.0); run `nook update`")
            || stderr(&output).contains(&format!(
                "warning: nook 99.0.0 is available (installed {}); run `nook update`",
                env!("CARGO_PKG_VERSION")
            ))
    );
    fs::remove_dir_all(home).unwrap();
}

#[test]
fn stale_cache_fetches_once_until_ttl_is_valid() {
    let home = test_home("ttl");
    write_cache(
        &home,
        unix_ms_now().saturating_sub(48 * 60 * 60 * 1000),
        "0.0.1",
    );
    let state = Arc::new(Mutex::new(ReleaseServer {
        tag: "v99.0.0".into(),
        archive: Vec::new(),
        checksum: String::new(),
        latest_status: 200,
        hang: false,
        hits: 0,
    }));
    let url = spawn_server(Arc::clone(&state));
    let first = nook(
        &home,
        &["config", "path"],
        &[("NOOK_UPDATE_RELEASES_URL", &url)],
    );
    assert!(first.status.success(), "{}", stderr(&first));
    assert!(stderr(&first).contains("nook 99.0.0 is available"));
    assert_eq!(state.lock().unwrap().hits, 1);

    let second = nook(
        &home,
        &["config", "path"],
        &[("NOOK_UPDATE_RELEASES_URL", &url)],
    );
    assert!(second.status.success(), "{}", stderr(&second));
    assert!(stderr(&second).contains("nook 99.0.0 is available"));
    assert_eq!(state.lock().unwrap().hits, 1);
    fs::remove_dir_all(home).unwrap();
}

#[test]
fn disabled_check_and_http_errors_never_block_commands() {
    let home = test_home("disable");
    let hanging = Arc::new(Mutex::new(ReleaseServer {
        tag: "v99.0.0".into(),
        archive: Vec::new(),
        checksum: String::new(),
        latest_status: 200,
        hang: true,
        hits: 0,
    }));
    let hang_url = spawn_server(Arc::clone(&hanging));
    let started = Instant::now();
    let disabled = nook(
        &home,
        &["config", "path"],
        &[
            ("NOOK_UPDATE_RELEASES_URL", &hang_url),
            ("NOOK_DISABLE_UPDATE_CHECK", "1"),
        ],
    );
    assert!(disabled.status.success(), "{}", stderr(&disabled));
    assert!(!stderr(&disabled).contains("warning: nook"));
    assert!(started.elapsed() < Duration::from_millis(400));
    assert_eq!(hanging.lock().unwrap().hits, 0);

    let failing = Arc::new(Mutex::new(ReleaseServer {
        tag: "v99.0.0".into(),
        archive: Vec::new(),
        checksum: String::new(),
        latest_status: 500,
        hang: false,
        hits: 0,
    }));
    let fail_url = spawn_server(Arc::clone(&failing));
    let errored = nook(
        &home,
        &["config", "path"],
        &[("NOOK_UPDATE_RELEASES_URL", &fail_url)],
    );
    assert!(errored.status.success(), "{}", stderr(&errored));
    assert!(!stderr(&errored).contains("warning: nook"));
    fs::remove_dir_all(home).unwrap();
}

#[test]
fn update_refuses_development_and_cargo_binaries() {
    let home = test_home("refuse");
    let development = Command::new(env!("CARGO_BIN_EXE_nook"))
        .args(["update"])
        .env("HOME", &home)
        .env("XDG_CONFIG_HOME", home.join("config"))
        .env("XDG_STATE_HOME", home.join("state"))
        .env("XDG_CACHE_HOME", home.join("cache"))
        .env("NOOK_DISABLE_UPDATE_CHECK", "1")
        .output()
        .unwrap();
    assert!(!development.status.success());
    assert!(stderr(&development).contains("development binary"));

    let cargo_bin = home.join(".cargo/bin");
    fs::create_dir_all(&cargo_bin).unwrap();
    let cargo_nook = copy_nook(&cargo_bin);
    let cargo = Command::new(&cargo_nook)
        .args(["update"])
        .env("HOME", &home)
        .env("XDG_CONFIG_HOME", home.join("config"))
        .env("XDG_STATE_HOME", home.join("state"))
        .env("XDG_CACHE_HOME", home.join("cache"))
        .env("NOOK_DISABLE_UPDATE_CHECK", "1")
        .output()
        .unwrap();
    assert!(!cargo.status.success(), "{}", stderr(&cargo));
    assert!(stderr(&cargo).contains("cargo install ntnook --locked --force"));

    let foreign_dir = home.join("opt/tools/bin");
    fs::create_dir_all(&foreign_dir).unwrap();
    let foreign = copy_nook(&foreign_dir);
    let unknown = Command::new(&foreign)
        .args(["update"])
        .env("HOME", &home)
        .env("XDG_CONFIG_HOME", home.join("config"))
        .env("XDG_STATE_HOME", home.join("state"))
        .env("XDG_CACHE_HOME", home.join("cache"))
        .env("NOOK_DISABLE_UPDATE_CHECK", "1")
        .output()
        .unwrap();
    assert!(!unknown.status.success(), "{}", stderr(&unknown));
    assert!(stderr(&unknown).contains("was not installed by the Nook installer"));
    fs::remove_dir_all(home).unwrap();
}

#[test]
fn update_replaces_a_managed_binary_from_a_verified_archive() {
    let home = test_home("replace");
    let install_dir = home.join(".local/bin");
    fs::create_dir_all(&install_dir).unwrap();
    let installed = copy_nook(&install_dir);

    let (archive, checksum) = pack_replacement(&home);
    let state = Arc::new(Mutex::new(ReleaseServer {
        tag: "v99.0.0".into(),
        archive,
        checksum,
        latest_status: 200,
        hang: false,
        hits: 0,
    }));
    let url = spawn_server(Arc::clone(&state));
    let output = Command::new("sh")
        .args([
            "-c",
            "umask 077; exec \"$0\" \"$@\"",
            installed.to_str().unwrap(),
            "update",
        ])
        .env("HOME", &home)
        .env("XDG_CONFIG_HOME", home.join("config"))
        .env("XDG_STATE_HOME", home.join("state"))
        .env("XDG_CACHE_HOME", home.join("cache"))
        .env("NOOK_UPDATE_RELEASES_URL", &url)
        .output()
        .unwrap();
    assert!(output.status.success(), "{}", stderr(&output));
    assert!(stdout(&output).contains("updated nook from"));
    let mode = fs::metadata(&installed).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o755);
    let replaced = Command::new(&installed).output().unwrap();
    assert!(replaced.status.success(), "{}", stderr(&replaced));
    assert_eq!(stdout(&replaced).trim(), "nook 99.0.0");
    fs::remove_dir_all(home).unwrap();
}

#[test]
fn update_replaces_a_binary_in_nook_install_dir() {
    let home = test_home("install-dir");
    let install_dir = home.join("custom/bin");
    fs::create_dir_all(&install_dir).unwrap();
    let installed = copy_nook(&install_dir);
    let (archive, checksum) = pack_replacement(&home);
    let state = Arc::new(Mutex::new(ReleaseServer {
        tag: "v99.0.0".into(),
        archive,
        checksum,
        latest_status: 200,
        hang: false,
        hits: 0,
    }));
    let url = spawn_server(Arc::clone(&state));
    let output = Command::new(&installed)
        .args(["update"])
        .env("HOME", &home)
        .env("XDG_CONFIG_HOME", home.join("config"))
        .env("XDG_STATE_HOME", home.join("state"))
        .env("XDG_CACHE_HOME", home.join("cache"))
        .env("NOOK_INSTALL_DIR", &install_dir)
        .env("NOOK_UPDATE_RELEASES_URL", &url)
        .output()
        .unwrap();
    assert!(output.status.success(), "{}", stderr(&output));
    assert!(stdout(&output).contains("updated nook from"));
    fs::remove_dir_all(home).unwrap();
}

fn copy_nook(directory: &Path) -> PathBuf {
    let installed = directory.join("nook");
    fs::copy(env!("CARGO_BIN_EXE_nook"), &installed).unwrap();
    let mut permissions = fs::metadata(&installed).unwrap().permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&installed, permissions).unwrap();
    installed
}

fn pack_replacement(home: &Path) -> (Vec<u8>, String) {
    let payload = home.join("payload");
    fs::create_dir_all(&payload).unwrap();
    let binary = payload.join("nook");
    fs::write(&binary, "#!/bin/sh\necho 'nook 99.0.0'\n").unwrap();
    let mut permissions = fs::metadata(&binary).unwrap().permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&binary, permissions).unwrap();
    let archive_path = home.join(ARCHIVE_NAME);
    let tar = Command::new("tar")
        .args(["--create", "--xz", "--file"])
        .arg(&archive_path)
        .arg("-C")
        .arg(&payload)
        .arg("nook")
        .status()
        .unwrap();
    assert!(tar.success(), "failed to create release archive");
    let checksum = Command::new("sha256sum")
        .current_dir(home)
        .arg(ARCHIVE_NAME)
        .output()
        .unwrap();
    assert!(checksum.status.success());
    (
        fs::read(&archive_path).unwrap(),
        String::from_utf8(checksum.stdout).unwrap(),
    )
}

fn spawn_server(state: Arc<Mutex<ReleaseServer>>) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let base = format!("http://{address}");
    let releases = format!("{base}/releases/latest");
    thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else {
                continue;
            };
            let mut state = state.lock().unwrap();
            state.hits += 1;
            if state.hang {
                drop(state);
                thread::sleep(Duration::from_secs(3));
                continue;
            }
            serve_request(&mut stream, &base, &state);
        }
    });
    releases
}

fn serve_request(stream: &mut TcpStream, base: &str, state: &ReleaseServer) {
    let request = read_request(stream);
    let path = request
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .unwrap_or("/");
    if path.ends_with("/releases/latest") {
        if state.latest_status != 200 {
            write_response(stream, state.latest_status, "text/plain", b"");
            return;
        }
        let body = format!(
            r#"{{"tag_name":"{}","assets":[{{"name":"{ARCHIVE_NAME}","browser_download_url":"{base}/{ARCHIVE_NAME}"}},{{"name":"{CHECKSUM_NAME}","browser_download_url":"{base}/{CHECKSUM_NAME}"}}]}}"#,
            state.tag
        );
        write_response(stream, 200, "application/json", body.as_bytes());
        return;
    }
    if path.ends_with(ARCHIVE_NAME) {
        write_response(stream, 200, "application/octet-stream", &state.archive);
        return;
    }
    if path.ends_with(CHECKSUM_NAME) {
        write_response(stream, 200, "text/plain", state.checksum.as_bytes());
        return;
    }
    write_response(stream, 404, "text/plain", b"not found");
}

fn read_request(stream: &mut TcpStream) -> String {
    let mut request = Vec::new();
    let mut buffer = [0_u8; 4096];
    loop {
        let read = stream.read(&mut buffer).unwrap();
        if read == 0 {
            break;
        }
        request.extend_from_slice(&buffer[..read]);
        if request.windows(4).any(|bytes| bytes == b"\r\n\r\n") {
            break;
        }
    }
    String::from_utf8_lossy(&request).into_owned()
}

fn write_response(stream: &mut TcpStream, status: u16, content_type: &str, body: &[u8]) {
    let reason = match status {
        200 => "OK",
        404 => "Not Found",
        500 => "Internal Server Error",
        _ => "Error",
    };
    write!(
        stream,
        "HTTP/1.1 {status} {reason}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    )
    .unwrap();
    stream.write_all(body).unwrap();
}

fn nook(home: &Path, arguments: &[&str], extra: &[(&str, &str)]) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_nook"));
    command
        .args(arguments)
        .env("HOME", home)
        .env("XDG_CONFIG_HOME", home.join("config"))
        .env("XDG_STATE_HOME", home.join("state"))
        .env("XDG_CACHE_HOME", home.join("cache"));
    for (key, value) in extra {
        command.env(key, value);
    }
    command.output().unwrap()
}

fn write_cache(home: &Path, checked_at_unix_ms: u64, latest: &str) {
    let directory = home.join("cache/nook");
    fs::create_dir_all(&directory).unwrap();
    fs::write(
        directory.join("update-check.json"),
        format!(r#"{{"checked_at_unix_ms":{checked_at_unix_ms},"latest":"{latest}"}}"#),
    )
    .unwrap();
}

fn test_home(label: &str) -> PathBuf {
    let directory =
        std::env::temp_dir().join(format!("nook-cli-update-{label}-{}", Uuid::new_v4()));
    fs::create_dir_all(directory.join("config/nook")).unwrap();
    fs::create_dir_all(directory.join("state")).unwrap();
    fs::create_dir_all(directory.join("cache")).unwrap();
    directory
}

fn unix_ms_now() -> u64 {
    u64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis(),
    )
    .unwrap()
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}
