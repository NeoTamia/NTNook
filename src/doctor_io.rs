//! Bounded observation transports. Workers isolate blocking system DNS and TLS.
#[cfg(unix)]
use std::io::Write;
use std::io::{self, Read};
use std::net::{TcpStream, ToSocketAddrs};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

pub(crate) const LIMIT: usize = 16 * 1024 * 1024;

thread_local! { static DEADLINE: std::cell::Cell<Option<Instant>> = const { std::cell::Cell::new(None) }; }

pub(crate) struct Deadline(Option<Instant>);
impl Deadline {
    pub(crate) fn enter(deadline: Instant) -> Self {
        Self(DEADLINE.replace(Some(deadline)))
    }
}
impl Drop for Deadline {
    fn drop(&mut self) {
        DEADLINE.set(self.0);
    }
}
pub(crate) fn git_output(command: &mut Command) -> io::Result<std::process::Output> {
    if let Some(deadline) = DEADLINE.get() {
        let timeout = deadline
            .checked_duration_since(Instant::now())
            .ok_or_else(|| io::Error::new(io::ErrorKind::TimedOut, "diagnostic budget exhausted"))?
            .min(Duration::from_secs(3));
        let stdout = output(command, timeout, 1024 * 1024)?;
        Ok(std::process::Output {
            status: std::process::ExitStatus::default(),
            stdout,
            stderr: Vec::new(),
        })
    } else {
        crate::operation::git_output(command)
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) enum Probe {
    Caddy(String),
    Dns(String),
    Tcp(String, u16),
    Http {
        url: String,
        address: Option<std::net::IpAddr>,
        ca_cert: Option<std::path::PathBuf>,
    },
    Mdns(String),
}

pub(crate) fn output(
    command: &mut Command,
    timeout: Duration,
    limit: usize,
) -> io::Result<Vec<u8>> {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let mut child = command.spawn()?;
    let mut pipe = child.stdout.take().expect("piped stdout");
    let (send, receive) = mpsc::sync_channel(1);
    std::thread::spawn(move || {
        let mut bytes = Vec::new();
        let result = pipe
            .by_ref()
            .take(limit as u64 + 1)
            .read_to_end(&mut bytes)
            .and_then(|_| {
                if bytes.len() > limit {
                    Err(io::Error::other("observation output exceeded limit"))
                } else {
                    Ok(bytes)
                }
            });
        let _ = send.send(result);
    });
    let deadline = Instant::now() + timeout;
    let result = (|| {
        let bytes = receive
            .recv_timeout(timeout)
            .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "observation timed out"))??;
        loop {
            if let Some(status) = child.try_wait()? {
                return if status.success() {
                    Ok(bytes)
                } else {
                    Err(io::Error::other("observation command failed"))
                };
            }
            if Instant::now() >= deadline {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "observation timed out",
                ));
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    })();
    if result.is_err() {
        let _ = child.kill();
    }
    let _ = child.wait();
    result
}

pub(crate) fn probe(probe: Probe, timeout: Duration) -> io::Result<Value> {
    let mut command = Command::new(std::env::current_exe()?);
    command
        .arg("__doctor_probe")
        .arg(serde_json::to_string(&probe)?);
    let bytes = output(&mut command, timeout, LIMIT)?;
    let result: Result<Value, String> = serde_json::from_slice(&bytes)?;
    result.map_err(io::Error::other)
}

pub(crate) fn worker() -> io::Result<()> {
    let input = std::env::args()
        .nth(2)
        .ok_or_else(|| io::Error::other("missing probe"))?;
    let probe: Probe = serde_json::from_str(&input)?;
    let result = observe(probe).map_err(|error| error.to_string());
    serde_json::to_writer(io::stdout().lock(), &result)?;
    Ok(())
}

fn agent_config(tls: ureq::tls::TlsConfig) -> ureq::config::Config {
    ureq::Agent::config_builder()
        .proxy(None)
        .max_redirects(0)
        .max_redirects_will_error(false)
        .http_status_as_error(false)
        .timeout_global(Some(Duration::from_secs(3)))
        .tls_config(tls)
        .build()
}

#[derive(Debug)]
struct FixedResolver(std::net::IpAddr);
impl ureq::unversioned::resolver::Resolver for FixedResolver {
    fn resolve(
        &self,
        uri: &ureq::http::Uri,
        _: &ureq::config::Config,
        _: ureq::unversioned::transport::NextTimeout,
    ) -> Result<ureq::unversioned::resolver::ResolvedSocketAddrs, ureq::Error> {
        let port = uri
            .port_u16()
            .unwrap_or(if uri.scheme_str() == Some("https") {
                443
            } else {
                80
            });
        let mut addresses = self.empty();
        addresses.push(std::net::SocketAddr::new(self.0, port));
        Ok(addresses)
    }
}

fn tls_config(ca_cert: Option<&std::path::Path>) -> io::Result<ureq::tls::TlsConfig> {
    use ureq::tls::{RootCerts, TlsConfig};
    let mut builder = TlsConfig::builder();
    #[cfg(windows)]
    {
        builder = builder
            .provider(ureq::tls::TlsProvider::NativeTls)
            .root_certs(RootCerts::PlatformVerifier);
    }
    let mut roots = Vec::new();
    #[cfg(unix)]
    for path in [
        "/etc/ssl/certs/ca-certificates.crt",
        "/etc/pki/ca-trust/extracted/pem/tls-ca-bundle.pem",
        "/etc/ssl/ca-bundle.pem",
    ] {
        if let Ok(bytes) = std::fs::read(path) {
            for item in ureq::tls::parse_pem(&bytes) {
                if let Ok(ureq::tls::PemItem::Certificate(cert)) = item {
                    roots.push(cert);
                }
            }
        }
    }
    if let Some(path) = ca_cert {
        roots.push(
            ureq::tls::Certificate::from_pem(&std::fs::read(path)?).map_err(io::Error::other)?,
        );
    }
    if !roots.is_empty() {
        builder = builder.root_certs(RootCerts::new_with_certs(&roots));
    }
    Ok(builder.build())
}

fn observe(probe: Probe) -> io::Result<Value> {
    match probe {
        Probe::Caddy(admin) => caddy(&admin),
        Probe::Dns(host) => {
            let mut addresses: Vec<_> = (host.as_str(), 0)
                .to_socket_addrs()?
                .map(|a| a.ip())
                .collect();
            addresses.sort();
            addresses.dedup();
            Ok(json!(addresses))
        }
        Probe::Tcp(host, port) => {
            let mut last = io::Error::other("no resolved addresses");
            for address in (host.as_str(), port).to_socket_addrs()? {
                match TcpStream::connect_timeout(&address, Duration::from_millis(500)) {
                    Ok(_) => return Ok(json!({"address": address.to_string()})),
                    Err(error) => last = error,
                }
            }
            Err(last)
        }
        Probe::Http {
            url,
            address,
            ca_cert,
        } => {
            let config = agent_config(tls_config(ca_cert.as_deref())?);
            let agent = match address {
                Some(ip) => ureq::Agent::with_parts(
                    config,
                    ureq::unversioned::transport::DefaultConnector::default(),
                    FixedResolver(ip),
                ),
                None => config.into(),
            };
            let response = agent.head(&url).call().map_err(|error| {
                io::Error::other(match error {
                    ureq::Error::Tls(_) => "TLS validation failed",
                    _ => "HTTP transport or TLS validation failed",
                })
            })?;
            Ok(
                json!({"status": response.status().as_u16(), "address": address, "trust_store": if ca_cert.is_some() {"explicit certificate (not proof of OS trust)"} else if cfg!(windows) {"native client"} else {"known system PEM bundles, falling back to ureq roots"}}),
            )
        }
        Probe::Mdns(host) => {
            let daemon = mdns_sd::ServiceDaemon::new().map_err(io::Error::other)?;
            let receiver = daemon
                .resolve_hostname(&format!("{host}."), Some(1500))
                .map_err(io::Error::other)?;
            let deadline = Instant::now() + Duration::from_millis(1600);
            let mut found = false;
            while let Some(left) = deadline.checked_duration_since(Instant::now()) {
                match receiver.recv_timeout(left) {
                    Ok(mdns_sd::HostnameResolutionEvent::AddressesFound(_, addresses)) => {
                        found = !addresses.is_empty();
                        break;
                    }
                    Ok(_) => {}
                    Err(_) => break,
                }
            }
            let _ = daemon.shutdown();
            Ok(json!({"observed": found, "vantage": "local host only"}))
        }
    }
}

fn caddy(admin: &str) -> io::Result<Value> {
    // Validate using the operational client's rules, but never use its PKI GET.
    crate::caddy::Client::new(admin).map_err(io::Error::other)?;
    #[cfg(unix)]
    if let Some(path) = admin
        .strip_prefix("unix://")
        .or_else(|| admin.strip_prefix("unix/"))
    {
        use std::os::unix::net::UnixStream;
        let mut stream = UnixStream::connect(path)?;
        stream.set_read_timeout(Some(Duration::from_secs(2)))?;
        stream.set_write_timeout(Some(Duration::from_secs(2)))?;
        stream
            .write_all(b"GET /config/ HTTP/1.0\r\nHost: localhost\r\nConnection: close\r\n\r\n")?;
        let mut response = Vec::new();
        stream.take(LIMIT as u64 + 1).read_to_end(&mut response)?;
        if response.len() > LIMIT {
            return Err(io::Error::other("Caddy response exceeded limit"));
        }
        let split = response
            .windows(4)
            .position(|v| v == b"\r\n\r\n")
            .ok_or_else(|| io::Error::other("invalid Caddy HTTP response"))?;
        let headers = String::from_utf8_lossy(&response[..split]);
        if !headers
            .lines()
            .next()
            .is_some_and(|line| line.split_whitespace().nth(1) == Some("200"))
        {
            return Err(io::Error::other("Caddy Admin API rejected GET /config/"));
        }
        let etag = headers
            .lines()
            .filter_map(|line| line.split_once(':'))
            .find(|(key, _)| key.eq_ignore_ascii_case("etag"))
            .map(|(_, value)| value.trim());
        let config: Value = serde_json::from_slice(&response[split + 4..])?;
        return Ok(json!({"config": config, "etag": etag}));
    }
    let url = url::Url::parse(admin)
        .map_err(io::Error::other)?
        .join("/config/")
        .map_err(io::Error::other)?;
    let agent: ureq::Agent = agent_config(tls_config(None)?).into();
    let mut response = agent
        .get(url.as_str())
        .call()
        .map_err(|_| io::Error::other("Caddy Admin API connection or TLS failed"))?;
    if response.status().as_u16() != 200 {
        return Err(io::Error::other(format!(
            "Caddy Admin API HTTP {}",
            response.status().as_u16()
        )));
    }
    let etag = response
        .headers()
        .get("etag")
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);
    let config: Value = response
        .body_mut()
        .with_config()
        .limit(LIMIT as u64)
        .read_json()
        .map_err(io::Error::other)?;
    Ok(json!({"config": config, "etag": etag}))
}
