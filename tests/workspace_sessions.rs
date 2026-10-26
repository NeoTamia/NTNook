//! Session detection must fail closed before ordinary config/Caddy processing.
use std::fs;
use std::path::Path;
use std::process::{Command, Output};

fn session_run(directory: &Path, marker: &std::ffi::OsStr, arguments: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_nook"))
        .current_dir(directory)
        .env("XDG_CONFIG_HOME", directory.join("config"))
        .env("XDG_STATE_HOME", directory.join("state"))
        .env("NOOK_DISABLE_UPDATE_CHECK", "1")
        .env("NOOK_WORKSPACE_SESSION", marker)
        .args(arguments)
        .output()
        .unwrap()
}

#[test]
fn supplied_invalid_session_never_becomes_a_standalone_run() {
    let directory =
        std::env::temp_dir().join(format!("nook-invalid-session-{}", uuid::Uuid::new_v4()));
    fs::create_dir_all(directory.join("config/nook")).unwrap();
    // A member gets its config from the session, even if global config is broken.
    fs::write(directory.join("config/nook/config.toml"), "invalid = [").unwrap();
    for marker in [
        std::ffi::OsString::new(),
        directory.join("missing/context.json").into_os_string(),
        std::ffi::OsString::from("relative/context.json"),
    ] {
        let output = session_run(&directory, &marker, &["run", "--", "must-never-start"]);
        assert!(!output.status.success());
        let error = String::from_utf8_lossy(&output.stderr);
        assert!(error.contains("workspace session"), "{error}");
        assert!(!error.contains("invalid configuration"), "{error}");
        assert!(!directory.join("state/nook/state.json").exists());
    }
    fs::remove_dir_all(directory).unwrap();
}

#[test]
fn external_runner_requires_a_workspace_and_its_own_command() {
    let directory = std::env::temp_dir().join(format!("nook-runner-cli-{}", uuid::Uuid::new_v4()));
    fs::create_dir(&directory).unwrap();
    for arguments in [
        vec!["run", "--runner", "--", "must-never-start"],
        vec!["run", "--runner"],
        vec!["run", "--runner", "--only", "api", "--", "must-never-start"],
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_nook"))
            .current_dir(&directory)
            .env("XDG_CONFIG_HOME", directory.join("config"))
            .env("XDG_STATE_HOME", directory.join("state"))
            .env("NOOK_DISABLE_UPDATE_CHECK", "1")
            .env_remove("NOOK_WORKSPACE_SESSION")
            .args(arguments)
            .output()
            .unwrap();
        assert!(!output.status.success());
        let error = String::from_utf8_lossy(&output.stderr);
        assert!(error.contains("--runner"), "{error}");
        assert!(!directory.join("state/nook/state.json").exists());
    }
    fs::remove_dir_all(directory).unwrap();
}

#[cfg(unix)]
#[path = "support/caddy.rs"]
mod caddy;

#[cfg(unix)]
mod shutdown {
    use super::caddy;
    use super::*;
    use serde_json::Value;
    use std::fs::File;
    use std::process::{Child, ExitStatus, Stdio};
    use std::thread;
    use std::time::{Duration, Instant};

    const SERVER: &str = r#"
import http.server, os, pathlib, signal, subprocess, sys, time
role = sys.argv[1]
def record(file, value):
    path = pathlib.Path(file)
    temporary = path.with_suffix('.tmp')
    temporary.write_text(value)
    temporary.replace(path)
def stop(sig, frame):
    def repeated(sig, frame):
        pathlib.Path(role + '-duplicate').write_text(str(sig))
    signal.signal(signal.SIGINT, repeated)
    signal.signal(signal.SIGTERM, repeated)
    record(role + '-stopping', str(sig))
    time.sleep(.35 if role == 'task' else .2)
    record(role + '-clean', str(sig))
    sys.exit(0)
for sig in [signal.SIGINT, signal.SIGTERM]:
    signal.signal(sig, signal.SIG_IGN if os.environ.get('STUBBORN') and role == 'api' else stop)
if role != 'task':
    server = http.server.HTTPServer((os.environ['HOST'], int(os.environ['PORT'])), http.server.SimpleHTTPRequestHandler)
    detached = subprocess.Popen([sys.executable, str(pathlib.Path(__file__).with_name('detached.py')), role], start_new_session=True)
    record(role + '-detached', str(detached.pid))
if os.environ.get('NONDUMPABLE') and role == 'api':
    import ctypes
    assert ctypes.CDLL(None).prctl(4, 0, 0, 0, 0) == 0
record(role + '-pid', str(os.getpid()))
if os.environ.get('TEST_TTY'):
    record(role + '-tty', str(int(os.isatty(0))))
while True:
    if pathlib.Path('read-stdin').exists():
        os.read(0, 1)
    if pathlib.Path('exit-api').exists() and role == 'api':
        sys.exit(23)
    time.sleep(.01)
"#;

    const DETACHED: &str = r#"
import os, pathlib, signal, sys, time
role = sys.argv[1]
def stop(sig, frame):
    def repeated(sig, frame):
        pathlib.Path(role + '-detached-duplicate').write_text(str(sig))
    signal.signal(signal.SIGINT, repeated)
    signal.signal(signal.SIGTERM, repeated)
    pathlib.Path(role + '-detached-stopping').write_text(str(sig))
    time.sleep(.6)
    pathlib.Path(role + '-detached-clean').write_text(str(sig))
    sys.exit(0)
for sig in [signal.SIGINT, signal.SIGTERM]:
    signal.signal(sig, stop if os.environ.get('GRACEFUL_DETACHED') else signal.SIG_IGN)
pathlib.Path(role + '-detached-ready').write_text(str(os.getpid()))
while True:
    time.sleep(.01)
"#;

    const RUNNER: &str = r#"
import os, pathlib, signal, subprocess, sys, time
root = pathlib.Path.cwd()
def stop(sig, frame):
    def repeated(sig, frame):
        pathlib.Path('runner-duplicate').write_text(str(sig))
    signal.signal(signal.SIGINT, repeated)
    signal.signal(signal.SIGTERM, repeated)
    # A parallel runner waits briefly for its foreground tasks to handle Ctrl-C.
    time.sleep(.5 if os.environ.get('TEST_TTY') else .05)
    pathlib.Path('runner-clean').write_text(str(sig))
    sys.exit(0)
for sig in [signal.SIGINT, signal.SIGTERM]:
    signal.signal(sig, stop)
pathlib.Path('context').write_text(os.environ['NOOK_WORKSPACE_SESSION'])
pathlib.Path('runner-pgid').write_text(str(os.getpgrp()))
def member(role):
    argv = [sys.executable, str(root/'server.py'), role]
    if os.environ.get('SHELL_MEMBER'):
        argv = ['/bin/sh', '-c', 'trap "exit 130" INT; trap "exit 143" TERM; "$@" & wait "$!"', 'nook-test-shell', *argv]
    return subprocess.Popen([os.environ['TEST_NOOK'], 'run', '--', *argv], cwd=root/role)
children = {role: member(role) for role in ['api', 'web']}
task = subprocess.Popen([sys.executable, 'server.py', 'task'], start_new_session=not os.environ.get('TEST_TTY'))
while True:
    protected = root/'api/api-pid'
    if os.environ.get('NONDUMPABLE') and protected.exists() and not (root/'api-unreadable').exists():
        pid = int(protected.read_text())
        try:
            pathlib.Path('/proc/'+str(pid)+'/environ').read_bytes()
        except PermissionError:
            (root/'api-unreadable').write_text('verified')
        else:
            raise AssertionError('nondumpable server environment must be unreadable')
    for role, child in children.items():
        code = child.poll()
        if code is not None:
            temporary = pathlib.Path(role + '-exit.tmp')
            temporary.write_text(str(code))
            temporary.replace(role + '-exit')
    if pathlib.Path('restart-api').exists():
        for file in ['api-exit', 'api/api-pid', 'api/api-clean', 'api/exit-api']:
            (root/file).unlink(missing_ok=True)
        children['api'] = member('api')
        pathlib.Path('restart-api').unlink()
    if pathlib.Path('exit-runner').exists():
        sys.exit(7)
    time.sleep(.01)
"#;

    struct Fixture {
        caddy: caddy::CaddyHarness,
        child: Child,
        terminal: Option<File>,
    }

    impl Fixture {
        fn start(stubborn: bool) -> Self {
            Self::start_with(stubborn, false, false)
        }

        fn start_with(stubborn: bool, nondumpable: bool, shell: bool) -> Self {
            Self::start_options(stubborn, nondumpable, shell, false, false)
        }

        #[allow(unsafe_code)]
        fn start_options(
            stubborn: bool,
            nondumpable: bool,
            shell: bool,
            tty: bool,
            detached: bool,
        ) -> Self {
            use std::os::fd::FromRawFd;
            use std::os::unix::process::CommandExt;

            let caddy = caddy::CaddyHarness::start_on_standard_ports();
            let root = caddy.root();
            fs::create_dir_all(root.join("config/nook")).unwrap();
            fs::write(
                root.join("config/nook/config.toml"),
                format!(
                    "format_version = 1\ncaddy_admin = {:?}\n",
                    caddy.admin_url()
                ),
            )
            .unwrap();
            fs::write(root.join("server.py"), SERVER).unwrap();
            fs::write(root.join("detached.py"), DETACHED).unwrap();
            fs::write(root.join("runner.py"), RUNNER).unwrap();
            let mut manifest = "format_version = 2\nname = 'grace'\n".to_owned();
            for role in ["api", "web"] {
                fs::create_dir(root.join(role)).unwrap();
                manifest.push_str(&format!("[[apps]]\nid = '{role}'\ndir = '{role}'\ntls = false\nframework = 'none'\ncommand = ['unused']\n"));
            }
            fs::write(root.join("nook.toml"), manifest).unwrap();
            let mut command = if nondumpable {
                // Match the protected-process recovery test: no exec may regain
                // root's capabilities inside the test's user namespace.
                let mut command = Command::new("python3");
                command.args([
                    "-c",
                    r"import ctypes,os,sys
libc=ctypes.CDLL(None)
assert libc.prctl(38,1,0,0,0)==0
header=(ctypes.c_uint32*2)(0x20080522,0)
data=(ctypes.c_uint32*6)()
assert libc.capset(ctypes.byref(header),ctypes.byref(data))==0
os.execv(sys.argv[1],sys.argv[1:])",
                ]);
                command.arg(env!("CARGO_BIN_EXE_nook"));
                command
            } else {
                Command::new(env!("CARGO_BIN_EXE_nook"))
            };
            let terminal = tty.then(|| {
                let (mut master, mut slave) = (-1, -1);
                assert_eq!(
                    unsafe {
                        libc::openpty(
                            &mut master,
                            &mut slave,
                            std::ptr::null_mut(),
                            std::ptr::null(),
                            std::ptr::null(),
                        )
                    },
                    0
                );
                let (master, slave) =
                    unsafe { (File::from_raw_fd(master), File::from_raw_fd(slave)) };
                command.stdin(Stdio::from(slave));
                // The child becomes a real controlling-terminal session leader.
                unsafe {
                    command.pre_exec(move || {
                        if libc::setsid() == -1
                            || libc::ioctl(libc::STDIN_FILENO, libc::TIOCSCTTY, 0) == -1
                        {
                            return Err(std::io::Error::last_os_error());
                        }
                        Ok(())
                    });
                }
                master
            });
            let child = command
                .current_dir(root)
                .env("XDG_CONFIG_HOME", root.join("config"))
                .env("XDG_STATE_HOME", root.join("state"))
                .env("NOOK_DISABLE_UPDATE_CHECK", "1")
                .env("TEST_NOOK", env!("CARGO_BIN_EXE_nook"))
                .env_remove("NOOK_WORKSPACE_SESSION")
                .envs(stubborn.then_some(("STUBBORN", "1")))
                .envs(nondumpable.then_some(("NONDUMPABLE", "1")))
                .envs(shell.then_some(("SHELL_MEMBER", "1")))
                .envs(tty.then_some(("TEST_TTY", "1")))
                .envs(detached.then_some(("GRACEFUL_DETACHED", "1")))
                .args(["run", "--runner", "--", "python3", "runner.py"])
                .stdout(Stdio::from(File::create(root.join("run.stdout")).unwrap()))
                .stderr(Stdio::from(File::create(root.join("run.stderr")).unwrap()))
                .spawn()
                .unwrap();
            let mut fixture = Self {
                caddy,
                child,
                terminal,
            };
            fixture.until(|fixture| {
                [
                    "api/api-pid",
                    "web/web-pid",
                    "task-pid",
                    "api/api-detached-ready",
                    "web/web-detached-ready",
                ]
                .iter()
                .all(|file| fixture.path(file).exists())
                    && (!tty
                        || ["api/api-tty", "web/web-tty", "task-tty"]
                            .iter()
                            .all(|file| fixture.path(file).exists()))
            });
            fixture
        }

        fn path(&self, file: &str) -> std::path::PathBuf {
            self.caddy.root().join(file)
        }
        fn logs(&self) -> String {
            fs::read_to_string(self.path("run.stderr")).unwrap()
        }
        fn until(&mut self, mut check: impl FnMut(&Self) -> bool) {
            let deadline = Instant::now() + Duration::from_secs(8);
            while !check(self) {
                assert!(
                    self.child.try_wait().unwrap().is_none(),
                    "root exited: {}",
                    self.logs()
                );
                assert!(
                    Instant::now() < deadline,
                    "fixture timed out: {}",
                    self.logs()
                );
                thread::sleep(Duration::from_millis(10));
            }
        }
        fn pid(&self, file: &str) -> i32 {
            fs::read_to_string(self.path(file))
                .unwrap()
                .parse()
                .unwrap()
        }
        fn state(&self) -> Value {
            serde_json::from_slice(&fs::read(self.path("state/nook/state.json")).unwrap()).unwrap()
        }
        fn context(&self) -> Value {
            let path = fs::read_to_string(self.path("context")).unwrap();
            serde_json::from_slice(&fs::read(path).unwrap()).unwrap()
        }
        fn wrapper(&self) -> i32 {
            self.context()["members"]
                .as_array()
                .unwrap()
                .iter()
                .find(|member| member["app_id"] == "api")
                .unwrap()["wrapper"]["pid"]
                .as_i64()
                .unwrap() as i32
        }
        fn assert_detached_cleanup(&self, sig: i32) {
            assert_eq!(
                fs::read_to_string(self.path("api/api-detached-clean")).ok(),
                Some(sig.to_string()),
                "detached descendant missed graceful cleanup: {}",
                self.logs()
            );
            for file in ["api/api-duplicate", "api/api-detached-duplicate"] {
                assert!(!self.path(file).exists(), "duplicate signal: {file}");
            }
        }
        fn wait(&mut self) -> ExitStatus {
            let deadline = Instant::now() + Duration::from_secs(7);
            loop {
                if let Some(status) = self.child.try_wait().unwrap() {
                    return status;
                }
                assert!(
                    Instant::now() < deadline,
                    "shutdown was not bounded: {}",
                    self.logs()
                );
                thread::sleep(Duration::from_millis(10));
            }
        }
        fn clean(&self) {
            let state = self.state();
            for key in ["leases", "workspace_groups"] {
                assert!(
                    state
                        .get(key)
                        .is_none_or(|value| value.as_object().unwrap().is_empty()),
                    "{key} survived shutdown: {state}\n{}",
                    self.logs()
                );
            }
            assert!(
                state["pending_operations"].as_array().unwrap().is_empty(),
                "pending operations survived shutdown: {state}\n{}",
                self.logs()
            );
            let context = fs::read_to_string(self.path("context")).unwrap();
            assert!(!Path::new(&context).exists(), "session survived shutdown");
            let config = self.caddy.config_json();
            for role in ["api", "web"] {
                assert!(
                    !config.contains(&format!("{role}.grace.localhost")),
                    "route survived shutdown"
                );
                for file in [
                    format!("{role}/{role}-pid"),
                    format!("{role}/{role}-detached"),
                ] {
                    assert!(!alive(self.pid(&file)), "{file} survived shutdown");
                }
            }
            assert!(
                !alive(self.pid("task-pid")),
                "non-HTTP task survived shutdown"
            );
            for file in [
                "api/api-duplicate",
                "web/web-duplicate",
                "task-duplicate",
                "runner-duplicate",
                "api/api-detached-duplicate",
                "web/web-detached-duplicate",
            ] {
                assert!(
                    !self.path(file).exists(),
                    "graceful signal forwarded twice to {file}"
                );
            }
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = self.child.kill();
            let _ = self.child.wait();
            for file in [
                "api/api-pid",
                "web/web-pid",
                "api/api-detached",
                "web/web-detached",
                "task-pid",
            ] {
                if self.path(file).exists() {
                    unsafe {
                        libc::kill(self.pid(file), libc::SIGKILL);
                    }
                }
            }
        }
    }
    fn alive(pid: i32) -> bool {
        if let Ok(stat) = fs::read_to_string(format!("/proc/{pid}/stat"))
            && stat
                .rsplit_once(") ")
                .is_some_and(|(_, fields)| fields.starts_with('Z'))
        {
            return false;
        }
        unsafe { libc::kill(pid, 0) == 0 }
    }
    fn signal(pid: i32, signal: i32) {
        assert_eq!(unsafe { libc::kill(pid, signal) }, 0);
    }
    fn namespace(test: &str) -> bool {
        if std::env::var_os("NOOK_SESSION_NETNS").is_some() {
            assert!(
                Command::new("ip")
                    .args(["link", "set", "lo", "up"])
                    .status()
                    .unwrap()
                    .success()
            );
            return false;
        }
        if std::env::var_os("NOOK_E2E_DIRECT").is_some() {
            return false;
        }
        let output = Command::new("unshare")
            .args(["--user", "--map-root-user", "--net"])
            .arg(std::env::current_exe().unwrap())
            .args(["--exact", test, "--nocapture"])
            .env("NOOK_SESSION_NETNS", "1")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        true
    }

    #[test]
    #[allow(unsafe_code)]
    fn tty_parallel_members_leave_foreground_with_runner_and_ctrl_c_stops_workspace() {
        use std::io::Write;
        use std::os::fd::AsRawFd;

        if namespace(
            "shutdown::tty_parallel_members_leave_foreground_with_runner_and_ctrl_c_stops_workspace",
        ) {
            return;
        }
        let mut fixture = Fixture::start_options(false, false, false, true, false);
        let runner = fixture.pid("runner-pgid");
        let foreground =
            || unsafe { libc::tcgetpgrp(fixture.terminal.as_ref().unwrap().as_raw_fd()) };
        assert_eq!(foreground(), runner, "a member stole the runner's terminal");
        for file in ["api/api-tty", "web/web-tty", "task-tty"] {
            assert_eq!(
                fs::read_to_string(fixture.path(file)).unwrap(),
                "1",
                "{file} must inherit stdin"
            );
        }
        let api = fixture.pid("api/api-pid");
        let web = fixture.pid("web/web-pid");
        assert_eq!(unsafe { libc::getpgid(api) }, api);
        assert_eq!(unsafe { libc::getpgid(web) }, web);
        assert_ne!(api, runner);
        assert_ne!(web, runner);
        signal(api, libc::SIGSTOP);
        fixture.until(|_| stopped(api));
        signal(api, libc::SIGCONT);
        fixture.until(|_| !stopped(api));
        thread::sleep(Duration::from_millis(100));
        assert_eq!(
            unsafe { libc::tcgetpgrp(fixture.terminal.as_ref().unwrap().as_raw_fd()) },
            runner,
            "resume stole the terminal"
        );
        // Background terminal reads stop only the member, never the runner.
        fs::write(fixture.path("api/read-stdin"), "").unwrap();
        fixture.until(|_| stopped(api));
        assert!(!stopped(runner), "background member stopped the runner");
        assert_eq!(
            unsafe { libc::tcgetpgrp(fixture.terminal.as_ref().unwrap().as_raw_fd()) },
            runner
        );
        // VINTR goes through the terminal driver, not kill(2).
        fixture.terminal.as_mut().unwrap().write_all(&[3]).unwrap();
        // The runner handles SIGINT and exits successfully; Nook preserves it.
        assert_eq!(fixture.wait().code(), Some(0));
        fixture.clean();
    }

    fn stopped(pid: i32) -> bool {
        fs::read_to_string(format!("/proc/{pid}/stat")).is_ok_and(|stat| {
            stat.rsplit_once(") ")
                .is_some_and(|(_, fields)| fields.starts_with('T'))
        })
    }

    #[test]
    fn stop_shell_member_preserves_tree_grace_and_same_port_restart() {
        if namespace("shutdown::stop_shell_member_preserves_tree_grace_and_same_port_restart") {
            return;
        }
        for (force, stubborn) in [(false, false), (true, false), (true, true)] {
            let mut fixture = Fixture::start_with(stubborn, false, true);
            let previous_pid = fixture.pid("api/api-pid");
            let state = fixture.state();
            let lease = state["leases"]
                .as_object()
                .unwrap()
                .values()
                .find(|lease| lease["hostname"] == "api.grace.localhost")
                .unwrap();
            let target = lease["target"].clone();
            assert_ne!(
                lease["pid"].as_i64(),
                Some(i64::from(previous_pid)),
                "must stop the shell leader"
            );
            let started = Instant::now();
            let mut command = Command::new(env!("CARGO_BIN_EXE_nook"));
            command
                .current_dir(fixture.caddy.root())
                .env("XDG_CONFIG_HOME", fixture.path("config"))
                .env("XDG_STATE_HOME", fixture.path("state"))
                .env("NOOK_DISABLE_UPDATE_CHECK", "1")
                .env_remove("NOOK_WORKSPACE_SESSION")
                .args(["stop", "api.grace.localhost"]);
            if force {
                command.arg("--force");
            }
            let output = command.output().unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            // While the async handler is running its claim must still exist.
            if !stubborn {
                fixture.until(|fixture| fixture.path("api/api-stopping").exists());
                let context = fs::read_to_string(fixture.path("context")).unwrap();
                let context: Value = serde_json::from_slice(&fs::read(context).unwrap()).unwrap();
                assert!(
                    context["members"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .any(|member| member["app_id"] == "api"),
                    "claim released before descendant cleanup"
                );
            }
            fixture.until(|fixture| fixture.path("api-exit").exists());
            assert!(
                started.elapsed() < Duration::from_secs(5),
                "member shutdown exceeded its bound"
            );
            assert_eq!(
                fixture.path("api/api-clean").exists(),
                !stubborn,
                "stop killed descendant before asynchronous cleanup"
            );
            assert!(!alive(previous_pid));
            assert!(!alive(fixture.pid("api/api-detached")));
            assert!(alive(fixture.pid("web/web-pid")));
            assert!(alive(fixture.pid("task-pid")));
            assert!(!fixture.path("runner-clean").exists());
            assert!(!fixture.path("web/web-clean").exists());
            fs::write(fixture.path("restart-api"), "").unwrap();
            fixture.until(|fixture| {
                fs::read_to_string(fixture.path("api/api-pid"))
                    .ok()
                    .is_some_and(|pid| pid.parse::<i32>().unwrap() != previous_pid)
            });
            assert!(fixture.state()["leases"].as_object().unwrap().values().any(
                |lease| lease["hostname"] == "api.grace.localhost" && lease["target"] == target
            ));
            // Root teardown must also finish when a member is already inside
            // its leader-exit grace, rather than depending on the wrapper exit.
            let restarted = fixture.pid("api/api-pid");
            fs::write(fixture.path("api/exit-api"), "").unwrap();
            fixture.until(|_| !alive(restarted));
            signal(fixture.child.id() as i32, libc::SIGTERM);
            assert_eq!(fixture.wait().code(), Some(143));
            fixture.clean();
        }
    }

    #[test]
    fn member_detached_cleanup_preserves_claim_status_port_and_siblings() {
        if namespace("shutdown::member_detached_cleanup_preserves_claim_status_port_and_siblings") {
            return;
        }
        for sig in [Some(libc::SIGINT), Some(libc::SIGTERM), None] {
            let mut fixture = Fixture::start_options(false, false, false, false, true);
            let previous_pid = fixture.pid("api/api-pid");
            let target = fixture.state()["leases"]
                .as_object()
                .unwrap()
                .values()
                .find(|lease| lease["hostname"] == "api.grace.localhost")
                .unwrap()["target"]
                .clone();
            let started = Instant::now();
            if let Some(sig) = sig {
                signal(fixture.wrapper(), sig);
            } else {
                fs::write(fixture.path("api/exit-api"), "").unwrap();
            }
            fixture.until(|fixture| {
                fixture.path("api/api-detached-stopping").exists()
                    || fixture.path("api-exit").exists()
            });
            assert!(
                fixture.path("api/api-detached-stopping").exists(),
                "detached descendant never received a graceful signal"
            );
            assert!(
                fixture.context()["members"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|member| member["app_id"] == "api"),
                "claim released during cleanup"
            );
            assert!(
                !fixture.path("api-exit").exists(),
                "wrapper exited during cleanup"
            );
            fixture.until(|fixture| fixture.path("api-exit").exists());
            assert!(started.elapsed() < Duration::from_secs(5));
            assert_eq!(
                fs::read_to_string(fixture.path("api-exit")).unwrap(),
                sig.map_or(23, |sig| 128 + sig).to_string()
            );
            fixture.assert_detached_cleanup(sig.unwrap_or(libc::SIGTERM));
            assert!(!alive(fixture.pid("api/api-detached")));
            assert!(alive(fixture.pid("web/web-pid")));
            assert!(alive(fixture.pid("web/web-detached")));
            assert!(!fixture.path("web/web-detached-stopping").exists());
            fs::write(fixture.path("restart-api"), "").unwrap();
            fixture.until(|fixture| {
                fs::read_to_string(fixture.path("api/api-pid"))
                    .ok()
                    .is_some_and(|pid| pid.parse::<i32>().unwrap() != previous_pid)
            });
            assert!(fixture.state()["leases"].as_object().unwrap().values().any(
                |lease| lease["hostname"] == "api.grace.localhost" && lease["target"] == target
            ));
            signal(fixture.child.id() as i32, libc::SIGTERM);
            assert_eq!(fixture.wait().code(), Some(143));
            fixture.clean();
        }
    }

    #[test]
    fn nook_stop_gives_detached_descendants_grace_without_resignaling_group() {
        if namespace(
            "shutdown::nook_stop_gives_detached_descendants_grace_without_resignaling_group",
        ) {
            return;
        }
        for shell in [false, true] {
            let mut fixture = Fixture::start_options(false, false, shell, false, true);
            let output = Command::new(env!("CARGO_BIN_EXE_nook"))
                .current_dir(fixture.caddy.root())
                .env("XDG_CONFIG_HOME", fixture.path("config"))
                .env("XDG_STATE_HOME", fixture.path("state"))
                .env("NOOK_DISABLE_UPDATE_CHECK", "1")
                .env_remove("NOOK_WORKSPACE_SESSION")
                .args(["stop", "api.grace.localhost"])
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            fixture.until(|fixture| fixture.path("api-exit").exists());
            assert_eq!(
                fs::read_to_string(fixture.path("api-exit")).unwrap(),
                if shell { "143" } else { "0" }
            );
            fixture.assert_detached_cleanup(libc::SIGTERM);
            assert!(alive(fixture.pid("web/web-pid")));
            assert!(alive(fixture.pid("web/web-detached")));
            assert!(!fixture.path("web/web-detached-stopping").exists());
            signal(fixture.child.id() as i32, libc::SIGTERM);
            assert_eq!(fixture.wait().code(), Some(143));
            fixture.clean();
        }
    }

    #[test]
    fn root_closing_during_member_grace_does_not_repeat_detached_signal() {
        if namespace("shutdown::root_closing_during_member_grace_does_not_repeat_detached_signal") {
            return;
        }
        for local in [false, true] {
            let mut fixture = Fixture::start_options(false, false, false, false, true);
            if local {
                signal(fixture.wrapper(), libc::SIGTERM);
                fixture.until(|fixture| {
                    fixture.path("api/api-detached-stopping").exists()
                        || fixture.path("api-exit").exists()
                });
                assert!(fixture.path("api/api-detached-stopping").exists());
            }
            signal(fixture.child.id() as i32, libc::SIGTERM);
            assert_eq!(fixture.wait().code(), Some(143));
            fixture.assert_detached_cleanup(libc::SIGTERM);
            assert_eq!(
                fs::read_to_string(fixture.path("web/web-detached-clean")).unwrap(),
                libc::SIGTERM.to_string()
            );
            fixture.clean();
        }
    }

    #[test]
    fn missing_context_still_cleans_the_members_owned_detached_processes() {
        if namespace("shutdown::missing_context_still_cleans_the_members_owned_detached_processes")
        {
            return;
        }
        let mut fixture = Fixture::start(false);
        let root = fixture.child.id() as i32;
        let context = fs::read_to_string(fixture.path("context")).unwrap();
        let lock = File::open(Path::new(&context).with_file_name("context.lock")).unwrap();
        lock.lock().unwrap();
        // Keep root recovery from masking a wrapper's missing-context cleanup.
        // Holding its session lock first also keeps the paused root out of it.
        signal(root, libc::SIGSTOP);
        fixture.until(|_| {
            fs::read_to_string(format!("/proc/{root}/stat"))
                .unwrap()
                .rsplit_once(") ")
                .is_some_and(|(_, fields)| fields.starts_with('T'))
        });
        let bytes = fs::read(&context).unwrap();
        let permissions = fs::metadata(&context).unwrap().permissions();
        fs::remove_file(&context).unwrap();
        drop(lock);
        fixture.until(|fixture| {
            fixture.path("api-exit").exists() && fixture.path("web-exit").exists()
        });
        let detached_survived = ["api/api-detached", "web/web-detached"]
            .iter()
            .any(|file| alive(fixture.pid(file)));
        assert_eq!(fs::read_to_string(fixture.path("api-exit")).unwrap(), "1");
        // Restore the private context so ordinary root teardown remains testable.
        fs::write(&context, bytes).unwrap();
        fs::set_permissions(&context, permissions).unwrap();
        signal(root, libc::SIGCONT);
        signal(root, libc::SIGTERM);
        assert_eq!(fixture.wait().code(), Some(143));
        fixture.clean();
        assert!(
            !detached_survived,
            "context loss skipped exact-token cleanup"
        );
    }

    #[test]
    fn member_signals_allow_async_cleanup_and_preserve_siblings() {
        if namespace("shutdown::member_signals_allow_async_cleanup_and_preserve_siblings") {
            return;
        }
        for (sig, stubborn) in [
            (Some(libc::SIGINT), false),
            (Some(libc::SIGTERM), false),
            (None, false),
            (Some(libc::SIGTERM), true),
        ] {
            let mut fixture = Fixture::start(stubborn);
            let previous_pid = fixture.pid("api/api-pid");
            let context = fs::read_to_string(fixture.path("context")).unwrap();
            let members: Value = serde_json::from_slice(&fs::read(&context).unwrap()).unwrap();
            let wrapper = members["members"]
                .as_array()
                .unwrap()
                .iter()
                .find(|member| member["app_id"] == "api")
                .unwrap()["wrapper"]["pid"]
                .as_u64()
                .unwrap();
            if let Some(sig) = sig {
                signal(wrapper as i32, sig);
            } else {
                fs::write(fixture.path("api/exit-api"), "").unwrap();
            }
            fixture.until(|fixture| fixture.path("api-exit").exists());
            assert_eq!(
                fs::read_to_string(fixture.path("api-exit")).unwrap(),
                sig.map_or(23, |sig| 128 + sig).to_string()
            );
            assert_eq!(
                fixture.path("api/api-clean").exists(),
                sig.is_some() && !stubborn,
                "member killed before asynchronous cleanup"
            );
            if let Some(sig) = sig
                && !stubborn
            {
                assert_eq!(
                    fs::read_to_string(fixture.path("api/api-clean")).unwrap(),
                    sig.to_string()
                );
            }
            assert!(
                !alive(fixture.pid("api/api-detached")),
                "claim released before detached cleanup"
            );
            assert!(
                alive(fixture.pid("web/web-pid")),
                "member shutdown killed its sibling"
            );
            assert!(!fixture.path("web/web-clean").exists());
            let context: Value = serde_json::from_slice(&fs::read(&context).unwrap()).unwrap();
            assert!(
                context["members"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .all(|member| member["app_id"] != "api")
            );
            fs::write(fixture.path("restart-api"), "").unwrap();
            fixture.until(|fixture| {
                fs::read_to_string(fixture.path("api/api-pid"))
                    .ok()
                    .is_some_and(|pid| pid.parse::<i32>().unwrap() != previous_pid)
            });
            assert!(
                alive(fixture.pid("web/web-pid")),
                "restart killed its sibling"
            );
            signal(fixture.child.id() as i32, libc::SIGTERM);
            assert_eq!(fixture.wait().code(), Some(143));
            fixture.clean();
        }
    }

    #[test]
    fn root_signals_allow_async_member_and_task_cleanup() {
        if namespace("shutdown::root_signals_allow_async_member_and_task_cleanup") {
            return;
        }
        for sig in [libc::SIGINT, libc::SIGTERM] {
            let mut fixture = Fixture::start(false);
            signal(fixture.child.id() as i32, sig);
            assert_eq!(fixture.wait().code(), Some(128 + sig));
            for file in [
                "api/api-clean",
                "web/web-clean",
                "task-clean",
                "runner-clean",
            ] {
                assert!(
                    fixture.path(file).exists(),
                    "{file}: root killed before asynchronous cleanup: {}",
                    fixture.logs()
                );
            }
            for file in [
                "api/api-clean",
                "web/web-clean",
                "task-clean",
                "runner-clean",
            ] {
                assert_eq!(
                    fs::read_to_string(fixture.path(file)).unwrap(),
                    sig.to_string()
                );
            }
            fixture.clean();
        }
    }

    #[test]
    fn member_shell_leader_exit_preserves_descendant_cleanup() {
        if namespace("shutdown::member_shell_leader_exit_preserves_descendant_cleanup") {
            return;
        }
        for sig in [Some(libc::SIGINT), Some(libc::SIGTERM), None] {
            let mut fixture = Fixture::start_with(false, false, true);
            let context = fs::read_to_string(fixture.path("context")).unwrap();
            let context: Value = serde_json::from_slice(&fs::read(context).unwrap()).unwrap();
            let wrapper = context["members"]
                .as_array()
                .unwrap()
                .iter()
                .find(|member| member["app_id"] == "api")
                .unwrap()["wrapper"]["pid"]
                .as_u64()
                .unwrap();
            let state = fixture.state();
            assert!(
                state["leases"].as_object().unwrap().values().any(|lease| {
                    lease["hostname"] == "api.grace.localhost"
                        && lease["pid"].as_i64() != Some(i64::from(fixture.pid("api/api-pid")))
                }),
                "the registered leader must be the shell, not the server: {state}"
            );
            if let Some(sig) = sig {
                signal(wrapper as i32, sig);
            } else {
                fs::write(fixture.path("api/exit-api"), "").unwrap();
            }
            fixture.until(|fixture| fixture.path("api-exit").exists());
            assert_eq!(
                fs::read_to_string(fixture.path("api-exit")).unwrap(),
                sig.map_or(23, |sig| 128 + sig).to_string()
            );
            if let Some(sig) = sig {
                assert!(
                    fixture.path("api/api-stopping").exists(),
                    "descendant never received the signal"
                );
                assert!(
                    fixture.path("api/api-clean").exists(),
                    "shell exited before descendant cleanup, and member forced the tree too soon: {}",
                    fixture.logs()
                );
                assert_eq!(
                    fs::read_to_string(fixture.path("api/api-clean")).unwrap(),
                    sig.to_string()
                );
            } else {
                assert!(
                    !fixture.path("api/api-clean").exists(),
                    "unsolicited exit must retain its normal status"
                );
            }
            assert!(
                !alive(fixture.pid("api/api-detached")),
                "detached child survived member cleanup"
            );
            assert!(
                alive(fixture.pid("web/web-pid")),
                "member shutdown killed its sibling"
            );
            signal(fixture.child.id() as i32, libc::SIGTERM);
            assert_eq!(fixture.wait().code(), Some(143));
            fixture.clean();
        }
    }

    #[test]
    fn root_shell_leader_exit_preserves_descendant_cleanup() {
        if namespace("shutdown::root_shell_leader_exit_preserves_descendant_cleanup") {
            return;
        }
        for sig in [libc::SIGINT, libc::SIGTERM] {
            let mut fixture = Fixture::start_with(false, false, true);
            signal(fixture.child.id() as i32, sig);
            assert_eq!(fixture.wait().code(), Some(128 + sig));
            for role in ["api", "web"] {
                assert!(
                    fixture.path(&format!("{role}/{role}-stopping")).exists(),
                    "descendant never received the signal"
                );
                assert!(
                    fixture.path(&format!("{role}/{role}-clean")).exists(),
                    "shell exited during root shutdown and member bypassed descendant grace: {}",
                    fixture.logs()
                );
                assert_eq!(
                    fs::read_to_string(fixture.path(&format!("{role}/{role}-clean"))).unwrap(),
                    sig.to_string()
                );
            }
            fixture.clean();
        }
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn registered_nondumpable_server_gets_one_graceful_signal() {
        if namespace("shutdown::registered_nondumpable_server_gets_one_graceful_signal") {
            return;
        }
        for sig in [libc::SIGINT, libc::SIGTERM] {
            let mut fixture = Fixture::start_with(false, true, false);
            fixture.until(|fixture| fixture.path("api-unreadable").exists());
            let state = fixture.state();
            assert!(
                state["leases"].as_object().unwrap().values().any(|lease| {
                    lease["hostname"] == "api.grace.localhost"
                        && lease["pid"].as_i64() == Some(i64::from(fixture.pid("api/api-pid")))
                }),
                "protected server must already have a durable lease: {state}"
            );
            signal(fixture.child.id() as i32, sig);
            assert_eq!(fixture.wait().code(), Some(128 + sig));
            assert!(
                fixture.path("api/api-clean").exists(),
                "verified child with unreadable environ missed graceful cleanup: {}",
                fixture.logs()
            );
            assert_eq!(
                fs::read_to_string(fixture.path("api/api-clean")).unwrap(),
                sig.to_string()
            );
            fixture.clean(); // Includes duplicate-signal and detached cleanup assertions.
        }
    }

    #[test]
    fn runner_exit_and_stubborn_member_shutdown_are_bounded() {
        if namespace("shutdown::runner_exit_and_stubborn_member_shutdown_are_bounded") {
            return;
        }
        for stubborn in [false, true] {
            let mut fixture = Fixture::start(stubborn);
            if stubborn {
                signal(fixture.child.id() as i32, libc::SIGTERM);
            } else {
                fs::write(fixture.path("exit-runner"), "").unwrap();
            }
            assert_eq!(fixture.wait().code(), Some(if stubborn { 143 } else { 7 }));
            assert!(
                fixture.path("web/web-clean").exists(),
                "orderly runner exit skipped member cleanup"
            );
            assert!(
                fixture.path("task-clean").exists(),
                "orderly runner exit skipped non-HTTP cleanup"
            );
            assert_eq!(fixture.path("api/api-clean").exists(), !stubborn);
            fixture.clean();
        }
    }
}
