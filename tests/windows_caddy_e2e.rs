#![cfg(windows)]

use std::fs;
use std::io::Read;
use std::net::{Ipv4Addr, TcpListener, TcpStream};
use std::os::windows::process::CommandExt;
use std::path::Path;
use std::process::{Child, Command, Output, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use uuid::Uuid;
use windows_sys::Win32::System::Threading::CREATE_NEW_CONSOLE;

struct CaddyChild(Child);

impl Drop for CaddyChild {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if self.0.try_wait().ok().flatten().is_some() {
                return;
            }
            thread::sleep(Duration::from_millis(50));
        }
    }
}

#[test]
fn native_caddy_supports_status_and_owned_aliases() {
    let root = std::env::temp_dir().join(format!("nook-windows-caddy-{}", Uuid::new_v4()));
    let app_data = root.join("roaming");
    let local_app_data = root.join("local");
    let caddy_data = root.join("caddy-data");
    fs::create_dir_all(app_data.join("Nook")).unwrap();
    fs::create_dir_all(&local_app_data).unwrap();
    fs::create_dir_all(&caddy_data).unwrap();

    let admin_guard = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let admin_port = admin_guard.local_addr().unwrap().port();
    let caddyfile = root.join("Caddyfile");
    fs::write(
        &caddyfile,
        format!(
            "{{\n\tadmin 127.0.0.1:{admin_port}\n\tauto_https disable_redirects\n\tskip_install_trust\n\tservers {{\n\t\tprotocols h1 h2\n\t}}\n}}\n\nhttps://localhost:443 {{\n\ttls internal\n\trespond 404\n}}\n"
        ),
    )
    .unwrap();
    fs::write(
        app_data.join("Nook/config.toml"),
        format!(
            "format_version = 1\ncaddy_admin = \"http://127.0.0.1:{admin_port}\"\nhttps_server = \"srv0\"\n"
        ),
    )
    .unwrap();

    drop(admin_guard);

    let child = Command::new("caddy.exe")
        .args(["run", "--config"])
        .arg(&caddyfile)
        .env("XDG_DATA_HOME", &caddy_data)
        .env("XDG_CONFIG_HOME", root.join("caddy-config"))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("caddy.exe must be available in PATH");
    let mut caddy = CaddyChild(child);
    wait_for_admin(admin_port, &mut caddy.0);

    let status = nook(&app_data, &local_app_data, &["status"]);
    assert!(
        status.status.success(),
        "status failed: {}",
        String::from_utf8_lossy(&status.stderr)
    );
    assert!(String::from_utf8_lossy(&status.stdout).contains("caddy\tok"));

    let set = nook(
        &app_data,
        &local_app_data,
        &["alias", "set", "windows-e2e", "3000"],
    );
    assert!(
        set.status.success(),
        "alias set failed: {}",
        String::from_utf8_lossy(&set.stderr)
    );
    let remove = nook(
        &app_data,
        &local_app_data,
        &["alias", "remove", "windows-e2e"],
    );
    assert!(
        remove.status.success(),
        "alias remove failed: {}",
        String::from_utf8_lossy(&remove.stderr)
    );

    let mut running = nook_command(&app_data, &local_app_data)
        .args([
            "run",
            "--name",
            "windows-stop",
            "--",
            "cmd.exe",
            "/D",
            "/C",
            "ping -n 31 127.0.0.1 >NUL",
        ])
        .creation_flags(CREATE_NEW_CONSOLE)
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let state_path = local_app_data.join("Nook/state.json");
    wait_for_state(&state_path, "windows-stop.localhost", &mut running);

    // The managed run owns a different console. Stopping it must therefore
    // use the named Job Object instead of GenerateConsoleCtrlEvent.
    let stop = nook(&app_data, &local_app_data, &["stop", "windows-stop"]);
    assert!(
        stop.status.success(),
        "stop from a separate process failed: {}",
        String::from_utf8_lossy(&stop.stderr)
    );
    wait_for_exit(&mut running, Duration::from_secs(10));

    drop(caddy);
    fs::remove_dir_all(root).unwrap();
}

fn nook(app_data: &Path, local_app_data: &Path, arguments: &[&str]) -> Output {
    let mut child = nook_command(app_data, local_app_data)
        .args(arguments)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(30);
    while Instant::now() < deadline {
        if child.try_wait().unwrap().is_some() {
            return child.wait_with_output().unwrap();
        }
        thread::sleep(Duration::from_millis(50));
    }
    let _ = child.kill();
    let output = child.wait_with_output().unwrap();
    panic!(
        "nook {arguments:?} timed out after 30 seconds; stdout: {}; stderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn nook_command(app_data: &Path, local_app_data: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_nook"));
    command
        .env("APPDATA", app_data)
        .env("LOCALAPPDATA", local_app_data)
        .env("NOOK_DISABLE_UPDATE_CHECK", "1");
    command
}

fn wait_for_state(path: &Path, hostname: &str, child: &mut Child) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        if fs::read_to_string(path).is_ok_and(|state| state.contains(hostname)) {
            return;
        }
        if let Some(status) = child.try_wait().unwrap() {
            panic!("nook run exited before writing its lease: {status}");
        }
        thread::sleep(Duration::from_millis(50));
    }
    panic!("nook run did not write the {hostname} lease");
}

fn wait_for_exit(child: &mut Child, timeout: Duration) {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if child.try_wait().unwrap().is_some() {
            return;
        }
        thread::sleep(Duration::from_millis(50));
    }
    let _ = child.kill();
    let mut stderr = String::new();
    if let Some(mut pipe) = child.stderr.take() {
        let _ = pipe.read_to_string(&mut stderr);
    }
    panic!("nook run did not exit after stop; stderr: {stderr}");
}

fn wait_for_admin(port: u16, child: &mut Child) {
    let deadline = Instant::now() + Duration::from_secs(15);
    while Instant::now() < deadline {
        if TcpStream::connect((Ipv4Addr::LOCALHOST, port)).is_ok() {
            return;
        }
        if let Some(status) = child.try_wait().unwrap() {
            let mut stderr = String::new();
            if let Some(mut pipe) = child.stderr.take() {
                let _ = pipe.read_to_string(&mut stderr);
            }
            panic!("Caddy exited before its Admin API was ready: {status}; stderr: {stderr}");
        }
        thread::sleep(Duration::from_millis(100));
    }
    panic!("Caddy Admin API did not become ready on port {port}");
}
