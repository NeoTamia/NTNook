//! ngrok CLI adapter. Account configuration stays outside Nook.
use crate::{
    process::{self, ForwardedSignals, ProcessSignal, RunError},
    reconcile::RouteBackend,
    state::{Lease, LeaseState, Scheme, Store, WorkspaceSupervisor},
    tunnel::{self, Phase, Registration, Tunnel, failure, wrapped},
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::{
    ffi::OsString,
    fs,
    net::{Ipv4Addr, TcpListener},
    path::{Path, PathBuf},
    time::{Duration, Instant},
};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct Settings {
    pub(crate) binary: String,
    pub(crate) config: Option<PathBuf>,
    pub(crate) startup_timeout_seconds: u64,
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            binary: "ngrok".into(),
            config: None,
            startup_timeout_seconds: 30,
        }
    }
}
impl Settings {
    pub(crate) fn validate(&self) -> Result<(), String> {
        if self.binary.is_empty() || !(1..=600).contains(&self.startup_timeout_seconds) {
            return Err("expected binary and startup_timeout_seconds in 1..=600".into());
        }
        Ok(())
    }
    fn config_path(&self) -> Result<Option<PathBuf>, RunError> {
        let path = if let Some(path) = &self.config {
            path.clone()
        } else {
            #[cfg(unix)]
            let path = std::env::var_os("HOME")
                .map(PathBuf::from)
                .map(|p| p.join(".config/ngrok/ngrok.yml"));
            #[cfg(windows)]
            let path = std::env::var_os("LOCALAPPDATA")
                .map(PathBuf::from)
                .map(|p| p.join("ngrok/ngrok.yml"));
            let Some(path) = path else { return Ok(None) };
            if !path.exists() {
                return Ok(None);
            }
            path
        };
        let path = fs::canonicalize(path).map_err(|_| {
            failure(
                "cannot read [ngrok].config; configure ngrok separately or supply NGROK_AUTHTOKEN",
            )
        })?;
        if path.to_string_lossy().contains(',') {
            return Err(failure(
                "ngrok --config does not support commas in configuration paths",
            ));
        }
        Ok(Some(path))
    }
}

pub(crate) fn validate_url(value: &str) -> Result<String, String> {
    let invalid = || {
        "expected an HTTPS origin on an ngrok domain, without credentials, port, path, query or fragment".to_owned()
    };
    let url = url::Url::parse(value).map_err(|_| invalid())?;
    let host = url.host_str().ok_or_else(invalid)?;
    if url.scheme() != "https"
        || !url.username().is_empty()
        || url.password().is_some()
        || url.port().is_some()
        || url.path() != "/"
        || url.query().is_some()
        || url.fragment().is_some()
        || ![
            ".ngrok.app",
            ".ngrok.dev",
            ".ngrok-free.app",
            ".ngrok-free.dev",
            ".ngrok.io",
        ]
        .iter()
        .any(|suffix| {
            host.strip_suffix(suffix).is_some_and(|label| {
                !label.is_empty()
                    && label
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'.')
            })
        })
    {
        return Err(invalid());
    }
    Ok(url.origin().ascii_serialization())
}

fn check(settings: &Settings, args: &[OsString]) -> Result<String, RunError> {
    let argv: Vec<_> = std::iter::once(OsString::from(&settings.binary))
        .chain(args.iter().cloned())
        .collect();
    let mut child = process::spawn_ngrok(&argv, Uuid::new_v4()).map_err(|_| failure("ngrok is not executable; install it explicitly or set [ngrok].binary (qualified CLI: 3.39.13)"))?;
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(code) = child.try_wait()? {
            child.join_output();
            if code != 0 {
                return Err(failure(
                    "ngrok configuration/capability check failed; run ngrok config check yourself; configuration v3 is required",
                ));
            }
            let output = child
                .auxiliary_output
                .lock()
                .map_err(|_| failure("cannot read ngrok capabilities"))?;
            return Ok(String::from_utf8_lossy(&output).into_owned());
        }
        if Instant::now() >= deadline {
            let _ = child.signal(ProcessSignal::Kill);
            let _ = child.wait();
            return Err(failure("ngrok capability check timed out"));
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}
pub(crate) fn preflight() -> Result<(), RunError> {
    let settings = crate::config::load_global().map_err(wrapped)?.ngrok;
    settings.validate().map_err(failure)?;
    let help = check(&settings, &["http".into(), "--help".into()])?;
    for flag in [
        "--name",
        "--url",
        "--config",
        "--traffic-policy-file",
        "--upstream-tls-verify",
        "--pooling-enabled",
    ] {
        if !help.contains(flag) {
            return Err(failure(
                "unsupported ngrok CLI; install a compatible v3 release (qualified CLI: 3.39.13)",
            ));
        }
    }
    if let Some(path) = settings.config_path()? {
        check(
            &settings,
            &[
                "config".into(),
                "check".into(),
                "--config".into(),
                path.into_os_string(),
            ],
        )?;
    }
    if settings.config_path()?.is_none() && std::env::var_os("NGROK_AUTHTOKEN").is_none() {
        return Err(failure(
            "ngrok authentication is not configured; configure ngrok separately or supply NGROK_AUTHTOKEN (never put tokens in nook.toml)",
        ));
    }
    Ok(())
}

fn upstream(value: &str) -> Result<url::Url, RunError> {
    let mut url = url::Url::parse(value).map_err(|_| failure("invalid ngrok upstream"))?;
    if !matches!(url.scheme(), "http" | "https")
        || !url.username().is_empty()
        || url.password().is_some()
        || url.path() != "/"
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(failure(
            "ngrok upstream must be a credential-free HTTP(S) loopback origin",
        ));
    }
    match url.host() {
        Some(url::Host::Ipv4(ip)) if ip.is_unspecified() => {
            url.set_host(Some("127.0.0.1")).map_err(wrapped)?;
        }
        Some(url::Host::Ipv6(ip)) if ip.is_unspecified() => {
            url.set_host(Some("[::1]")).map_err(wrapped)?;
        }
        Some(url::Host::Ipv4(ip)) if ip.is_loopback() => (),
        Some(url::Host::Ipv6(ip)) if ip.is_loopback() => (),
        Some(url::Host::Domain("localhost")) => (),
        _ => return Err(failure("ngrok V1 exposes loopback upstreams only")),
    }
    Ok(url)
}

// An alias borrows an existing owner; it never creates an application lease.
pub(crate) struct AliasOptions<'a> {
    pub(crate) preserve_host: bool,
    pub(crate) ca: Option<&'a Path>,
}

pub(crate) fn start(
    config: &crate::config::ResolvedRunConfig,
    owner_id: Uuid,
    target: &str,
    store: &Store,
    routes: &mut impl RouteBackend,
    alias: Option<AliasOptions<'_>>,
) -> Result<(Tunnel, String), RunError> {
    let settings = crate::config::load_global().map_err(wrapped)?.ngrok;
    let requested = config
        .ngrok_url
        .as_deref()
        .map(validate_url)
        .transpose()
        .map_err(failure)?;
    let target_url = upstream(target)?;
    let supervisor = process::read_process_identity(std::process::id())
        .ok_or_else(|| failure("cannot identify tunnel supervisor"))?;
    let mut registration = Registration {
        id: Uuid::new_v4(),
        provider: "ngrok".into(),
        alias: alias.is_some(),
        owner_id,
        hostname: config.hostname.clone(),
        target: target.into(),
        supervisor: WorkspaceSupervisor {
            pid: std::process::id(),
            process_start_time_ticks: supervisor.start_time_ticks,
        },
        child: None,
        url: requested.clone(),
        metrics_port: 0,
        phase: Phase::Preparing,
        committed: alias.is_some(),
    };
    let mut tunnel = Tunnel {
        id: registration.id,
        store: store.clone(),
        child: None,
        agent: tunnel::local_agent(),
        last_poll: Instant::now(),
        failures: 0,
    };
    let result = (|| {
        {
            let _guard = store.lock_operations()?;
            store.mutate(|registry| {
                if registry.tunnels.values().any(|r| r.phase != Phase::Stopping && (r.owner_id == owner_id || (requested.is_some() && r.url == requested))) {
                    return Err(crate::state::Error::Io { path: store.path().to_owned(), source: std::io::Error::other("ngrok URL or owner already has an active tunnel; stop it first (pooling is disabled)") });
                }
                registry.tunnels.insert(registration.id, registration.clone()); Ok(())
            })?;
        }
        let directory = registration.directory(store);
        crate::platform::create_private_directory(&directory).map_err(wrapped)?;
        if directory.to_string_lossy().contains(',') {
            return Err(failure("ngrok session paths cannot contain commas"));
        }
        let host = if alias.as_ref().is_some_and(|a| a.preserve_host) {
            config.hostname.clone()
        } else {
            target_url[url::Position::BeforeHost..url::Position::AfterPort].to_owned()
        };
        let policy = json!({"on_http_request":[{"actions":[{"type":"add-headers","config":{"headers":{"host":host}}}]}]});
        fs::write(
            directory.join("policy.json"),
            serde_json::to_vec(&policy).map_err(wrapped)?,
        )
        .map_err(wrapped)?;
        let user_config = settings.config_path()?;
        let deadline = Instant::now() + Duration::from_secs(settings.startup_timeout_seconds);
        let mut signals = ForwardedSignals::new().map_err(wrapped)?;
        // The CLI cannot inherit a listener. Retry bind failures only; never adopt a listener.
        for attempt in 0..3 {
            tunnel.require_starting(&mut signals)?;
            let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).map_err(wrapped)?;
            registration.metrics_port = listener.local_addr().map_err(wrapped)?.port();
            let overlay = directory.join("agent.yml");
            fs::write(&overlay, format!("version: 3\nagent:\n  web_addr: 127.0.0.1:{}\n  console_ui: false\n  log: stdout\n  log_format: json\n  update_check: false\n  remote_management: false\n", registration.metrics_port)).map_err(wrapped)?;
            let mut files = OsString::new();
            if let Some(path) = &user_config {
                files.push(path);
                files.push(",");
            }
            files.push(&overlay);
            check(
                &settings,
                &[
                    "config".into(),
                    "check".into(),
                    "--config".into(),
                    overlay.as_os_str().to_owned(),
                ],
            )?;
            let mut argv: Vec<OsString> = vec![
                settings.binary.clone().into(),
                "http".into(),
                target_url.to_string().into(),
                "--name".into(),
                format!("nook-{}", registration.id).into(),
                "--config".into(),
                files,
                "--inspect=false".into(),
                "--pooling-enabled=false".into(),
                "--traffic-policy-file".into(),
                directory.join("policy.json").into_os_string(),
                "--upstream-tls-verify=true".into(),
            ];
            if let Some(url) = &requested {
                argv.extend(["--url".into(), url.into()]);
            }
            if let Some(ca) = alias.as_ref().and_then(|a| a.ca) {
                argv.extend([
                    "--upstream-tls-verify-cas".into(),
                    ca.as_os_str().to_owned(),
                ]);
            }
            store.mutate(|registry| {
                if let Some(r) = registry.tunnels.get_mut(&registration.id) {
                    r.metrics_port = registration.metrics_port;
                }
                Ok(())
            })?;
            drop(listener);
            let child = process::spawn_ngrok(&argv, registration.id)?;
            let lease = Lease {
                id: registration.id,
                hostname: config.hostname.clone(),
                target: target.into(),
                scheme: Scheme::Http,
                tls: true,
                lan: false,
                worktree: config.worktree.clone(),
                group_id: None,
                workspace_supervisor: None,
                pid: child.pid,
                pgid: child.pgid,
                process_start_time_ticks: child.start_time_ticks,
                state: LeaseState::Starting,
            };
            tunnel.child = Some(child);
            store.mutate(|registry| {
                if let Some(r) = registry.tunnels.get_mut(&registration.id) {
                    r.child = Some(lease);
                }
                Ok(())
            })?;
            loop {
                tunnel.require_starting(&mut signals)?;
                let child = tunnel.child.as_mut().expect("spawned");
                if child.try_wait()?.is_some() {
                    child.join_output();
                    let output = child
                        .auxiliary_output
                        .lock()
                        .map_err(|_| failure("cannot inspect ngrok failure"))?;
                    let bind_failed =
                        String::from_utf8_lossy(&output).contains("address already in use");
                    if bind_failed && attempt < 2 {
                        break;
                    }
                    return Err(failure(
                        "ngrok exited before URL discovery; check authentication, domain ownership, account quotas and outbound HTTPS with ngrok directly (raw output suppressed)",
                    ));
                }
                if let Some(url) = discover(&tunnel.agent, &registration)? {
                    if requested.as_ref().is_some_and(|r| r != &url) {
                        return Err(failure("ngrok returned a different URL than requested"));
                    }
                    let _guard = store.lock_operations()?;
                    tunnel.require_starting(&mut signals)?;
                    store.mutate(|registry| {
                        if registry.tunnels.values().any(|r| {
                            r.id != registration.id
                                && r.url.as_ref() == Some(&url)
                                && r.phase != Phase::Stopping
                        }) {
                            return Err(crate::state::Error::Io {
                                path: store.path().to_owned(),
                                source: std::io::Error::other(
                                    "public URL is already owned by another tunnel",
                                ),
                            });
                        }
                        if let Some(r) = registry.tunnels.get_mut(&registration.id) {
                            r.url = Some(url.clone());
                            r.phase = Phase::Connected;
                        }
                        Ok(())
                    })?;
                    eprintln!(
                        "nook: domain={} tunnel_url={} tunnel=connected provider=ngrok (public, direct upstream)",
                        config.hostname, url
                    );
                    return Ok(url);
                }
                if Instant::now() >= deadline {
                    return Err(failure(
                        "ngrok startup timed out waiting for its owned endpoint; check API compatibility, authentication and connectivity",
                    ));
                }
                std::thread::sleep(Duration::from_millis(100));
            }
        }
        Err(failure(
            "ngrok local API port could not be bound after three attempts",
        ))
    })();
    match result {
        Ok(url) => Ok((tunnel, url)),
        Err(error) => {
            tunnel.stop();
            if let Ok(_guard) = store.lock_operations() {
                let _ = store.mutate(|registry| {
                    tunnel::reconcile(registry, store, routes);
                    Ok(())
                });
            }
            Err(error)
        }
    }
}

pub(crate) fn discover(
    agent: &ureq::Agent,
    entry: &Registration,
) -> Result<Option<String>, RunError> {
    let Ok(mut response) = agent
        .get(format!(
            "http://127.0.0.1:{}/api/endpoints",
            entry.metrics_port
        ))
        .call()
    else {
        return Ok(None);
    };
    if response.status() != 200 {
        return Ok(None);
    }
    let data: serde_json::Value = response
        .body_mut()
        .with_config()
        .limit(65536)
        .read_json()
        .map_err(|_| failure("invalid JSON from ngrok local API"))?;
    let endpoints = data
        .get("endpoints")
        .and_then(|v| v.as_array())
        .ok_or_else(|| failure("unsupported ngrok local API: expected endpoints"))?;
    let name = format!("nook-{}", entry.id);
    let matching: Vec<_> = endpoints
        .iter()
        .filter(|e| e.get("name").and_then(|v| v.as_str()) == Some(&name))
        .collect();
    if matching.is_empty() {
        return Ok(None);
    }
    if matching.len() != 1 {
        return Err(failure("ambiguous ngrok endpoint identity"));
    }
    let endpoint = matching[0];
    let actual = endpoint
        .pointer("/upstream/url")
        .and_then(|v| v.as_str())
        .ok_or_else(|| failure("ngrok endpoint has no upstream"))?;
    if upstream(actual)? != upstream(&entry.target)? {
        return Err(failure("ngrok endpoint upstream does not match its owner"));
    }
    let url = endpoint
        .get("url")
        .and_then(|v| v.as_str())
        .ok_or_else(|| failure("ngrok endpoint has no public URL"))?;
    validate_url(url).map(Some).map_err(failure)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn public_origins_have_exact_provider_boundaries() {
        assert_eq!(
            validate_url("https://DEMO.ngrok.app/").unwrap(),
            "https://demo.ngrok.app"
        );
        for value in [
            "http://demo.ngrok.app",
            "https://ngrok.app",
            "https://demo.ngrok.app.evil.test",
            "https://demo.ngrok.app:444",
            "https://user:password@demo.ngrok.app",
            "https://demo.ngrok.app/path",
            "https://demo.ngrok.app?token=secret",
            "https://*.ngrok.app",
            "https://demo.ngrok.app#fragment",
        ] {
            assert!(validate_url(value).is_err(), "accepted {value}");
        }
    }
    #[test]
    fn direct_targets_are_loopback_and_keep_tls_identity() {
        assert_eq!(
            upstream("http://0.0.0.0:3000").unwrap().as_str(),
            "http://127.0.0.1:3000/"
        );
        assert_eq!(
            upstream("http://[::]:3000").unwrap().as_str(),
            "http://[::1]:3000/"
        );
        assert_eq!(
            upstream("https://localhost:8443").unwrap().host_str(),
            Some("localhost")
        );
        for value in [
            "http://192.168.1.5:3000",
            "https://example.com",
            "http://localhost/a",
            "http://user:password@localhost",
            "file:///tmp/file",
        ] {
            assert!(upstream(value).is_err());
        }
    }
}
