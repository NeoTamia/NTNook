//! Caddy Admin API integration and canonical proxy targets.
#![allow(dead_code)]

use base64::Engine as _;
use serde_json::Value;
use serde_json::json;
use std::fmt;
#[cfg(unix)]
use std::fs;
#[cfg(unix)]
use std::io::{Read, Write};
#[cfg(unix)]
use std::os::unix::net::UnixStream;
#[cfg(unix)]
use std::path::{Path, PathBuf};
use std::time::Duration;
use url::Url;
use uuid::Uuid;

use crate::reconcile::{RouteBackend, RouteError, RouteSpec};

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Error {
    InvalidPort,
    InvalidUrl(String),
    UnsupportedScheme,
    MissingHost,
    Credentials,
    Path,
    Query,
    Fragment,
    AdminUrl(String),
    AdminRequest(String),
    InvalidConfig(&'static str),
    AmbiguousServer {
        kind: &'static str,
        candidates: Vec<String>,
    },
    InvalidOverride {
        kind: &'static str,
        name: String,
        candidates: Vec<String>,
    },
    ForeignHostname(String),
    ManagedHostname(String),
    MissingEtag,
    ConcurrentMutation,
    InvalidOwnedRoute,
    MissingSelectedServer(&'static str),
    InvalidLocalCa,
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidPort => write!(formatter, "alias target port must be between 1 and 65535"),
            Self::InvalidUrl(reason) => write!(formatter, "invalid alias target URL: {reason}"),
            Self::UnsupportedScheme => {
                write!(formatter, "alias target scheme must be http or https")
            }
            Self::MissingHost => write!(formatter, "alias target URL must contain a host"),
            Self::Credentials => write!(formatter, "alias target URL must not contain credentials"),
            Self::Path => write!(formatter, "alias target URL path must be empty or `/`"),
            Self::Query => write!(formatter, "alias target URL must not contain a query"),
            Self::Fragment => write!(formatter, "alias target URL must not contain a fragment"),
            Self::AdminUrl(reason) => write!(formatter, "invalid Caddy Admin API URL: {reason}"),
            Self::AdminRequest(reason) => {
                write!(
                    formatter,
                    "Caddy Admin API request failed: {reason}; check that Caddy is running and caddy_admin is correct"
                )
            }
            Self::InvalidConfig(reason) => {
                write!(formatter, "invalid Caddy configuration: {reason}")
            }
            Self::AmbiguousServer { kind, candidates } if candidates.is_empty() => {
                let listener = if *kind == "HTTPS" { ":443" } else { ":80" };
                write!(
                    formatter,
                    "expected exactly one {kind} server; detected: none. Configure a Caddy server listening explicitly on {listener}"
                )
            }
            Self::AmbiguousServer { kind, candidates } => write!(
                formatter,
                "expected exactly one {kind} server; detected: {}. Configure an explicit override",
                display_candidates(candidates)
            ),
            Self::InvalidOverride {
                kind,
                name,
                candidates,
            } => write!(
                formatter,
                "configured {kind} server `{name}` is not compatible; detected: {}",
                display_candidates(candidates)
            ),
            Self::ForeignHostname(hostname) => write!(
                formatter,
                "hostname `{hostname}` is already claimed by a foreign Caddy route; choose another hostname or update that route outside Nook"
            ),
            Self::ManagedHostname(hostname) => write!(
                formatter,
                "hostname `{hostname}` is already managed by Nook; use --force to replace it"
            ),
            Self::MissingEtag => write!(
                formatter,
                "Caddy response did not include an ETag required for a safe mutation"
            ),
            Self::ConcurrentMutation => write!(
                formatter,
                "Caddy configuration kept changing after three retries; retry the command"
            ),
            Self::InvalidOwnedRoute => write!(
                formatter,
                "Nook route container contains a route without a valid Nook owner marker"
            ),
            Self::MissingSelectedServer(kind) => write!(
                formatter,
                "no selected Caddy {kind} server is available for this route; configure a compatible listener or server override"
            ),
            Self::InvalidLocalCa => write!(
                formatter,
                "Caddy returned an invalid local CA certificate; inspect the `local` PKI authority"
            ),
        }
    }
}

impl std::error::Error for Error {}

fn display_candidates(candidates: &[String]) -> String {
    if candidates.is_empty() {
        "none".into()
    } else {
        candidates.join(", ")
    }
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Upstream {
    pub(crate) url: Url,
}

#[derive(Debug)]
enum AdminEndpoint {
    Http(Url),
    #[cfg(unix)]
    Unix {
        address: String,
        socket: PathBuf,
    },
}

#[derive(Debug)]
pub(crate) struct Client {
    admin: AdminEndpoint,
}

impl Client {
    pub(crate) fn new(admin: &str) -> Result<Self, Error> {
        if let Some(socket_path) = admin
            .strip_prefix("unix://")
            .or_else(|| admin.strip_prefix("unix/"))
        {
            #[cfg(windows)]
            return Err(Error::AdminUrl(format!(
                "Unix sockets are not supported on Windows (`{socket_path}`); use an HTTP(S) Admin API URL such as http://127.0.0.1:2019"
            )));
            #[cfg(unix)]
            {
                let socket = PathBuf::from(socket_path);
                if !socket.is_absolute() {
                    return Err(Error::AdminUrl(
                    "Unix socket path must be absolute (for example unix//run/caddy/admin.socket)"
                        .into(),
                ));
                }
                return Ok(Self {
                    admin: AdminEndpoint::Unix {
                        address: admin.to_owned(),
                        socket,
                    },
                });
            }
        }
        let admin = Url::parse(admin).map_err(|error| Error::AdminUrl(error.to_string()))?;
        if !matches!(admin.scheme(), "http" | "https") || admin.host().is_none() {
            return Err(Error::AdminUrl(
                "expected an absolute HTTP(S) URL or Unix socket address".into(),
            ));
        }
        Ok(Self {
            admin: AdminEndpoint::Http(admin),
        })
    }

    pub(crate) fn fetch_config(&self) -> Result<Value, Error> {
        match &self.admin {
            AdminEndpoint::Http(admin) => {
                let endpoint = admin
                    .join("/config/")
                    .map_err(|error| Error::AdminUrl(error.to_string()))?;
                let mut response = ureq::get(endpoint.as_str()).call().map_err(admin_request)?;
                response
                    .body_mut()
                    .read_json()
                    .map_err(|error| Error::AdminRequest(error.to_string()))
            }
            #[cfg(unix)]
            AdminEndpoint::Unix { socket, .. } => {
                let response = unix_request(socket, "GET", "/config/", &[], None)?;
                response.ensure_success()?;
                serde_json::from_slice(&response.body)
                    .map_err(|error| Error::AdminRequest(error.to_string()))
            }
        }
    }

    pub(crate) fn fetch_local_ca(&self) -> Result<String, Error> {
        let response = match &self.admin {
            AdminEndpoint::Http(admin) => {
                let endpoint = admin
                    .join("/pki/ca/local")
                    .map_err(|error| Error::AdminUrl(error.to_string()))?;
                let mut response = ureq::get(endpoint.as_str()).call().map_err(admin_request)?;
                response
                    .body_mut()
                    .read_to_string()
                    .map_err(|error| Error::AdminRequest(error.to_string()))
            }
            #[cfg(unix)]
            AdminEndpoint::Unix { socket, .. } => {
                let response = unix_request(socket, "GET", "/pki/ca/local", &[], None)?;
                response.ensure_success()?;
                String::from_utf8(response.body)
                    .map_err(|error| Error::AdminRequest(error.to_string()))
            }
        }?;
        parse_local_ca_response(&response)
    }

    pub(crate) fn trust_command(&self) -> String {
        let address = match &self.admin {
            AdminEndpoint::Http(admin) => {
                let host = admin.host_str().unwrap_or("127.0.0.1");
                let port = admin.port_or_known_default().unwrap_or(2019);
                if host.contains(':') {
                    format!("[{host}]:{port}")
                } else {
                    format!("{host}:{port}")
                }
            }
            #[cfg(unix)]
            AdminEndpoint::Unix { address, .. } => address.clone(),
        };
        format!("caddy trust --address {address}")
    }

    pub(crate) fn mutate_server_routes(
        &self,
        server: &str,
        transform: impl FnMut(Vec<Value>) -> Result<Vec<Value>, Error>,
    ) -> Result<(), Error> {
        match &self.admin {
            AdminEndpoint::Http(_) => {
                let endpoint = self.server_routes_http_endpoint(server)?;
                mutate_with_retry(
                    || {
                        let mut response =
                            ureq::get(endpoint.as_str()).call().map_err(admin_request)?;
                        let etag = response
                            .headers()
                            .get("etag")
                            .and_then(|value| value.to_str().ok())
                            .map(str::to_owned)
                            .ok_or(Error::MissingEtag)?;
                        let routes = response
                            .body_mut()
                            .read_json::<Vec<Value>>()
                            .map_err(|error| Error::AdminRequest(error.to_string()))?;
                        Ok((etag, routes))
                    },
                    |etag, routes| match ureq::patch(endpoint.as_str())
                        .header("If-Match", etag)
                        .send_json(routes)
                    {
                        Ok(_) => Ok(WriteOutcome::Applied),
                        Err(ureq::Error::StatusCode(412)) => Ok(WriteOutcome::PreconditionFailed),
                        Err(error) => Err(admin_request(error)),
                    },
                    transform,
                    std::thread::sleep,
                )
            }
            #[cfg(unix)]
            AdminEndpoint::Unix { socket, .. } => {
                let endpoint = server_routes_path(server);
                mutate_with_retry(
                    || {
                        let response = unix_request(socket, "GET", &endpoint, &[], None)?;
                        response.ensure_success()?;
                        let etag = response
                            .header("etag")
                            .ok_or(Error::MissingEtag)?
                            .to_owned();
                        let routes = serde_json::from_slice::<Vec<Value>>(&response.body)
                            .map_err(|error| Error::AdminRequest(error.to_string()))?;
                        Ok((etag, routes))
                    },
                    |etag, routes| {
                        let body = serde_json::to_vec(routes)
                            .map_err(|error| Error::AdminRequest(error.to_string()))?;
                        let response = unix_request(
                            socket,
                            "PATCH",
                            &endpoint,
                            &[("If-Match", etag), ("Content-Type", "application/json")],
                            Some(&body),
                        )?;
                        match response.status {
                            200..=299 => Ok(WriteOutcome::Applied),
                            412 => Ok(WriteOutcome::PreconditionFailed),
                            _ => Err(response.status_error()),
                        }
                    },
                    transform,
                    std::thread::sleep,
                )
            }
        }
    }

    fn server_routes_http_endpoint(&self, server: &str) -> Result<Url, Error> {
        #[cfg(windows)]
        let AdminEndpoint::Http(admin) = &self.admin;
        #[cfg(unix)]
        let admin = match &self.admin {
            AdminEndpoint::Http(admin) => admin,
            #[cfg(unix)]
            AdminEndpoint::Unix { .. } => {
                return Err(Error::AdminUrl("expected an HTTP(S) endpoint".into()));
            }
        };
        let mut endpoint = admin.clone();
        endpoint.set_path("");
        endpoint
            .path_segments_mut()
            .map_err(|()| Error::AdminUrl("URL cannot be a base".into()))?
            .extend(["config", "apps", "http", "servers", server, "routes"]);
        Ok(endpoint)
    }
}

fn parse_local_ca_response(response: &str) -> Result<String, Error> {
    if response.trim_start().starts_with('{') {
        let value: Value = serde_json::from_str(response)
            .map_err(|error| Error::AdminRequest(error.to_string()))?;
        return value
            .get("root_certificate")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .ok_or(Error::InvalidLocalCa);
    }
    Ok(response.to_owned())
}

fn server_routes_path(server: &str) -> String {
    let mut endpoint = Url::parse("http://localhost").expect("static URL is valid");
    endpoint
        .path_segments_mut()
        .expect("HTTP URL is a valid base")
        .extend(["config", "apps", "http", "servers", server, "routes"]);
    endpoint.path().to_owned()
}

#[cfg(unix)]
struct UnixResponse {
    status: u16,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

#[cfg(unix)]
impl UnixResponse {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(header, _)| header.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }

    fn ensure_success(&self) -> Result<(), Error> {
        if (200..=299).contains(&self.status) {
            Ok(())
        } else {
            Err(self.status_error())
        }
    }

    fn status_error(&self) -> Error {
        let body = String::from_utf8_lossy(&self.body);
        Error::AdminRequest(format!("HTTP status {}: {}", self.status, body.trim()))
    }
}

#[cfg(unix)]
fn unix_request(
    socket: &Path,
    method: &str,
    path: &str,
    headers: &[(&str, &str)],
    body: Option<&[u8]>,
) -> Result<UnixResponse, Error> {
    let mut stream =
        UnixStream::connect(socket).map_err(|error| unix_connect_error(socket, error))?;
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .map_err(|error| Error::AdminRequest(error.to_string()))?;
    stream
        .set_write_timeout(Some(Duration::from_secs(5)))
        .map_err(|error| Error::AdminRequest(error.to_string()))?;

    write!(
        stream,
        "{method} {path} HTTP/1.1\r\nHost: localhost\r\nAccept: application/json\r\nConnection: close\r\n"
    )
    .map_err(|error| Error::AdminRequest(error.to_string()))?;
    for (name, value) in headers {
        write!(stream, "{name}: {value}\r\n")
            .map_err(|error| Error::AdminRequest(error.to_string()))?;
    }
    if let Some(body) = body {
        write!(stream, "Content-Length: {}\r\n", body.len())
            .map_err(|error| Error::AdminRequest(error.to_string()))?;
    }
    stream
        .write_all(b"\r\n")
        .map_err(|error| Error::AdminRequest(error.to_string()))?;
    if let Some(body) = body {
        stream
            .write_all(body)
            .map_err(|error| Error::AdminRequest(error.to_string()))?;
    }

    let mut bytes = Vec::new();
    stream
        .read_to_end(&mut bytes)
        .map_err(|error| Error::AdminRequest(error.to_string()))?;
    parse_http_response(&bytes)
}

#[cfg(unix)]
fn unix_connect_error(socket: &Path, error: std::io::Error) -> Error {
    let remediation = if error.kind() == std::io::ErrorKind::PermissionDenied {
        format!(
            "; configure Caddy's admin listener as `unix/{}|0660` so its group permissions survive API configuration changes (an ExecStartPost chmod alone is not persistent)",
            socket.display()
        )
    } else {
        String::new()
    };
    Error::AdminRequest(format!(
        "cannot connect to Unix socket {}: {error}{remediation}",
        socket.display()
    ))
}

#[cfg(unix)]
fn parse_http_response(bytes: &[u8]) -> Result<UnixResponse, Error> {
    let header_end = bytes
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .ok_or_else(|| Error::AdminRequest("invalid response from Unix socket".into()))?;
    let head = std::str::from_utf8(&bytes[..header_end])
        .map_err(|error| Error::AdminRequest(error.to_string()))?;
    let mut lines = head.split("\r\n");
    let status = lines
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|status| status.parse().ok())
        .ok_or_else(|| Error::AdminRequest("invalid HTTP status from Unix socket".into()))?;
    let headers: Vec<(String, String)> = lines
        .map(|line| {
            line.split_once(':')
                .map(|(name, value)| (name.to_owned(), value.trim().to_owned()))
                .ok_or_else(|| Error::AdminRequest("invalid HTTP header from Unix socket".into()))
        })
        .collect::<Result<_, _>>()?;
    let raw_body = &bytes[header_end + 4..];
    let body = if headers.iter().any(|(name, value)| {
        name.eq_ignore_ascii_case("transfer-encoding") && value.eq_ignore_ascii_case("chunked")
    }) {
        decode_chunked(raw_body)?
    } else if let Some(length) = headers
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case("content-length"))
        .and_then(|(_, value)| value.parse::<usize>().ok())
    {
        raw_body
            .get(..length)
            .ok_or_else(|| Error::AdminRequest("truncated response from Unix socket".into()))?
            .to_vec()
    } else {
        raw_body.to_vec()
    };
    Ok(UnixResponse {
        status,
        headers,
        body,
    })
}

#[cfg(unix)]
fn decode_chunked(mut bytes: &[u8]) -> Result<Vec<u8>, Error> {
    let mut decoded = Vec::new();
    loop {
        let line_end = bytes
            .windows(2)
            .position(|window| window == b"\r\n")
            .ok_or_else(|| Error::AdminRequest("invalid chunked response".into()))?;
        let size = std::str::from_utf8(&bytes[..line_end])
            .ok()
            .and_then(|line| line.split(';').next())
            .and_then(|size| usize::from_str_radix(size.trim(), 16).ok())
            .ok_or_else(|| Error::AdminRequest("invalid chunk size".into()))?;
        bytes = &bytes[line_end + 2..];
        if size == 0 {
            return Ok(decoded);
        }
        let chunk = bytes
            .get(..size)
            .ok_or_else(|| Error::AdminRequest("truncated chunked response".into()))?;
        decoded.extend_from_slice(chunk);
        bytes = bytes
            .get(size + 2..)
            .filter(|_| bytes.get(size..size + 2) == Some(b"\r\n"))
            .ok_or_else(|| Error::AdminRequest("invalid chunk terminator".into()))?;
    }
}

#[cfg(unix)]
pub(crate) fn local_ca_is_trusted(pem: &str) -> Result<bool, Error> {
    let (canonical, _) = canonical_local_ca(pem)?;
    let certificate = pem_certificates(&canonical).remove(0);
    for path in [
        "/etc/ssl/certs/ca-certificates.crt",
        "/etc/pki/ca-trust/extracted/pem/tls-ca-bundle.pem",
        "/etc/ssl/ca-bundle.pem",
    ] {
        match fs::read_to_string(path) {
            Ok(bundle) if pem_certificates(&bundle).contains(&certificate) => return Ok(true),
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => {}
        }
    }
    Ok(false)
}

#[cfg(windows)]
#[allow(unsafe_code)]
pub(crate) fn local_ca_is_trusted(pem: &str) -> Result<bool, Error> {
    use windows_sys::Win32::Security::Cryptography::{
        CertCloseStore, CertEnumCertificatesInStore, CertFreeCertificateContext,
        CertOpenSystemStoreW,
    };

    let (_, expected) = canonical_local_ca(pem)?;
    let store_name: Vec<u16> = "ROOT".encode_utf16().chain(Some(0)).collect();
    let store = unsafe { CertOpenSystemStoreW(0, store_name.as_ptr()) };
    if store.is_null() {
        return Err(Error::AdminRequest(format!(
            "cannot open the Windows ROOT certificate store: {}",
            std::io::Error::last_os_error()
        )));
    }
    let mut previous = std::ptr::null();
    let mut trusted = false;
    loop {
        let context = unsafe { CertEnumCertificatesInStore(store, previous) };
        if context.is_null() {
            break;
        }
        let certificate = unsafe {
            std::slice::from_raw_parts(
                (*context).pbCertEncoded,
                usize::try_from((*context).cbCertEncoded).unwrap_or(0),
            )
        };
        if certificate == expected {
            trusted = true;
            unsafe { CertFreeCertificateContext(context) };
            break;
        }
        previous = context;
    }
    unsafe { CertCloseStore(store, 0) };
    Ok(trusted)
}

pub(crate) fn canonical_local_ca(pem: &str) -> Result<(String, Vec<u8>), Error> {
    let encoded = pem_certificates(pem)
        .into_iter()
        .next()
        .ok_or(Error::InvalidLocalCa)?;
    let der = base64::engine::general_purpose::STANDARD
        .decode(&encoded)
        .map_err(|_| Error::InvalidLocalCa)?;
    if der.is_empty() || der.first() != Some(&0x30) {
        return Err(Error::InvalidLocalCa);
    }
    let mut wrapped = String::new();
    for chunk in encoded.as_bytes().chunks(64) {
        wrapped.push_str(std::str::from_utf8(chunk).map_err(|_| Error::InvalidLocalCa)?);
        wrapped.push('\n');
    }
    Ok((
        format!("-----BEGIN CERTIFICATE-----\n{wrapped}-----END CERTIFICATE-----\n"),
        der,
    ))
}

fn pem_certificates(contents: &str) -> Vec<String> {
    const BEGIN: &str = "-----BEGIN CERTIFICATE-----";
    const END: &str = "-----END CERTIFICATE-----";
    let mut certificates = Vec::new();
    let mut rest = contents;
    while let Some(begin) = rest.find(BEGIN) {
        rest = &rest[begin + BEGIN.len()..];
        let Some(end) = rest.find(END) else {
            break;
        };
        certificates.push(
            rest[..end]
                .chars()
                .filter(|character| !character.is_whitespace())
                .collect(),
        );
        rest = &rest[end + END.len()..];
    }
    certificates
}

pub(crate) struct ManagedCaddyRoutes<'a> {
    pub(crate) client: &'a Client,
    pub(crate) https_server: Option<&'a str>,
    pub(crate) http_server: Option<&'a str>,
    pub(crate) loopback_host: &'a str,
    pub(crate) client_ip_ranges: &'a [String],
}

impl ManagedCaddyRoutes<'_> {
    fn server(&self, tls: bool) -> Result<(&str, &'static str), Error> {
        if tls {
            self.https_server
                .map(|server| (server, HTTPS_CONTAINER_ID))
                .ok_or(Error::MissingSelectedServer("HTTPS"))
        } else {
            self.http_server
                .map(|server| (server, HTTP_CONTAINER_ID))
                .ok_or(Error::MissingSelectedServer("HTTP"))
        }
    }
}

impl RouteBackend for ManagedCaddyRoutes<'_> {
    fn ensure(&mut self, route: &RouteSpec) -> Result<(), RouteError> {
        let (server, container_id) = self.server(route.tls).map_err(route_error)?;
        let upstream = normalize_upstream(&route.target).map_err(route_error)?;
        let owner_id = route.owner_id;
        let hostname = route.hostname.clone();
        let proxy = build_proxy_route_for_network(
            &owner_route_id(owner_id),
            &hostname,
            &upstream.url,
            route.preserve_host,
            self.loopback_host,
            self.client_ip_ranges,
        );
        self.client
            .mutate_server_routes(server, move |mut routes| {
                reject_foreign_hostname_claims(&routes, std::slice::from_ref(&hostname))?;
                reject_managed_hostname_claim(
                    &routes,
                    container_id,
                    &hostname,
                    owner_id,
                    route.replace_existing,
                )?;
                update_owned_route(
                    &mut routes,
                    container_id,
                    owner_id,
                    Some((hostname.clone(), proxy.clone())),
                )?;
                Ok(routes)
            })
            .map_err(route_error)
    }

    fn remove_if_owned(
        &mut self,
        _hostname: &str,
        owner_id: Uuid,
        tls: bool,
    ) -> Result<(), RouteError> {
        let (server, container_id) = self.server(tls).map_err(route_error)?;
        self.client
            .mutate_server_routes(server, move |mut routes| {
                update_owned_route(&mut routes, container_id, owner_id, None)?;
                Ok(routes)
            })
            .map_err(route_error)
    }
}

fn route_error(error: Error) -> RouteError {
    RouteError(error.to_string())
}

fn admin_request(error: impl fmt::Display) -> Error {
    Error::AdminRequest(error.to_string())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WriteOutcome {
    Applied,
    PreconditionFailed,
}

fn mutate_with_retry(
    mut read: impl FnMut() -> Result<(String, Vec<Value>), Error>,
    mut write: impl FnMut(&str, &Vec<Value>) -> Result<WriteOutcome, Error>,
    mut transform: impl FnMut(Vec<Value>) -> Result<Vec<Value>, Error>,
    mut sleep: impl FnMut(Duration),
) -> Result<(), Error> {
    const DELAYS: [Duration; 3] = [
        Duration::from_millis(25),
        Duration::from_millis(50),
        Duration::from_millis(100),
    ];
    for attempt in 0..=DELAYS.len() {
        let (etag, current) = read()?;
        let updated = transform(current)?;
        if write(&etag, &updated)? == WriteOutcome::Applied {
            return Ok(());
        }
        if let Some(delay) = DELAYS.get(attempt) {
            sleep(*delay);
        } else {
            return Err(Error::ConcurrentMutation);
        }
    }
    unreachable!()
}

#[derive(Debug, Default)]
pub(crate) struct ServerOverrides<'a> {
    pub(crate) https: Option<&'a str>,
    pub(crate) http: Option<&'a str>,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct ServerSelection {
    pub(crate) https: Option<String>,
    pub(crate) http: Option<String>,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct ManagedObservation {
    pub(crate) owner_id: Uuid,
    pub(crate) hostname: String,
    pub(crate) tls: bool,
}

#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct ManagedInspection {
    pub(crate) https_container: bool,
    pub(crate) http_container: bool,
    pub(crate) routes: Vec<ManagedObservation>,
}

pub(crate) fn inspect_managed(
    config: &Value,
    selection: &ServerSelection,
) -> Result<ManagedInspection, Error> {
    let servers = config
        .pointer("/apps/http/servers")
        .and_then(Value::as_object)
        .ok_or(Error::InvalidConfig("apps.http.servers is missing"))?;
    let mut inspection = ManagedInspection::default();
    for (server_name, container_id, tls) in [
        (selection.https.as_deref(), HTTPS_CONTAINER_ID, true),
        (selection.http.as_deref(), HTTP_CONTAINER_ID, false),
    ] {
        let Some(server_name) = server_name else {
            continue;
        };
        let server = servers
            .get(server_name)
            .ok_or(Error::InvalidConfig("selected server is missing"))?;
        let Some(routes) = server.get("routes") else {
            continue;
        };
        let routes = routes.as_array().ok_or(Error::InvalidConfig(
            "selected server routes are not an array",
        ))?;
        let Some(container) = routes
            .iter()
            .find(|route| route.get("@id").and_then(Value::as_str) == Some(container_id))
        else {
            continue;
        };
        if tls {
            inspection.https_container = true;
        } else {
            inspection.http_container = true;
        }
        for route in container
            .pointer("/handle/0/routes")
            .and_then(Value::as_array)
            .ok_or(Error::InvalidOwnedRoute)?
        {
            let owner_id = route
                .get("@id")
                .and_then(Value::as_str)
                .and_then(parse_owner_route_id)
                .ok_or(Error::InvalidOwnedRoute)?;
            let hostname = route
                .pointer("/match/0/host/0")
                .and_then(Value::as_str)
                .ok_or(Error::InvalidOwnedRoute)?
                .to_owned();
            inspection.routes.push(ManagedObservation {
                owner_id,
                hostname,
                tls,
            });
        }
    }
    inspection
        .routes
        .sort_by(|left, right| left.hostname.cmp(&right.hostname));
    Ok(inspection)
}

pub(crate) fn discover_servers(
    config: &Value,
    overrides: ServerOverrides<'_>,
    need_https: bool,
    need_http: bool,
) -> Result<ServerSelection, Error> {
    let https_candidates = server_candidates(config, 443)?;
    let http_candidates = server_candidates(config, 80)?;
    Ok(ServerSelection {
        https: need_https
            .then(|| select_server("HTTPS", overrides.https, &https_candidates))
            .transpose()?,
        http: need_http
            .then(|| select_server("HTTP", overrides.http, &http_candidates))
            .transpose()?,
    })
}

pub(crate) fn discover_available_servers(
    config: &Value,
    overrides: ServerOverrides<'_>,
) -> Result<ServerSelection, Error> {
    Ok(ServerSelection {
        https: select_optional_server("HTTPS", overrides.https, &server_candidates(config, 443)?)?,
        http: select_optional_server("HTTP", overrides.http, &server_candidates(config, 80)?)?,
    })
}

fn server_candidates(config: &Value, port: u16) -> Result<Vec<String>, Error> {
    let servers = config
        .pointer("/apps/http/servers")
        .and_then(Value::as_object)
        .ok_or(Error::InvalidConfig("apps.http.servers is missing"))?;
    let mut names: Vec<String> = servers
        .iter()
        .filter(|(_, server)| {
            server
                .get("listen")
                .and_then(Value::as_array)
                .is_some_and(|listeners| {
                    listeners
                        .iter()
                        .filter_map(Value::as_str)
                        .any(|listener| listener_port(listener) == Some(port))
                })
        })
        .map(|(name, _)| name.clone())
        .collect();
    names.sort();
    Ok(names)
}

fn select_optional_server(
    kind: &'static str,
    override_name: Option<&str>,
    candidates: &[String],
) -> Result<Option<String>, Error> {
    if override_name.is_some() || candidates.len() > 1 {
        return select_server(kind, override_name, candidates).map(Some);
    }
    Ok(candidates.first().cloned())
}

fn select_server(
    kind: &'static str,
    override_name: Option<&str>,
    candidates: &[String],
) -> Result<String, Error> {
    if let Some(name) = override_name {
        return candidates
            .iter()
            .find(|candidate| candidate.as_str() == name)
            .cloned()
            .ok_or_else(|| Error::InvalidOverride {
                kind,
                name: name.into(),
                candidates: candidates.to_vec(),
            });
    }
    match candidates {
        [name] => Ok(name.clone()),
        _ => Err(Error::AmbiguousServer {
            kind,
            candidates: candidates.to_vec(),
        }),
    }
}

fn listener_port(listener: &str) -> Option<u16> {
    listener.rsplit(':').next()?.parse().ok()
}

pub(crate) const HTTPS_CONTAINER_ID: &str = "nook_https_routes_v1";
pub(crate) const HTTP_CONTAINER_ID: &str = "nook_http_routes_v1";

#[derive(Debug, Clone)]
pub(crate) struct ManagedRoute {
    pub(crate) hostname: String,
    pub(crate) no_tls: bool,
    pub(crate) route: Value,
}

pub(crate) fn build_containers(routes: &[ManagedRoute]) -> (Option<Value>, Option<Value>) {
    (
        build_container(
            HTTPS_CONTAINER_ID,
            routes.iter().filter(|route| !route.no_tls),
        ),
        build_container(
            HTTP_CONTAINER_ID,
            routes.iter().filter(|route| route.no_tls),
        ),
    )
}

fn build_container<'a>(id: &str, routes: impl Iterator<Item = &'a ManagedRoute>) -> Option<Value> {
    let routes: Vec<_> = routes.collect();
    if routes.is_empty() {
        return None;
    }
    let mut hostnames: Vec<_> = routes.iter().map(|route| route.hostname.clone()).collect();
    hostnames.sort();
    hostnames.dedup();
    let children: Vec<_> = routes
        .into_iter()
        .map(|route| route.route.clone())
        .collect();
    Some(json!({
        "@id": id,
        "match": [{ "host": hostnames }],
        "handle": [{ "handler": "subroute", "routes": children }]
    }))
}

pub(crate) fn place_container(server_routes: &mut Vec<Value>, id: &str, container: Option<Value>) {
    server_routes.retain(|route| route.get("@id").and_then(Value::as_str) != Some(id));
    let Some(container) = container else {
        return;
    };
    let insertion = server_routes
        .iter()
        .position(is_catch_all)
        .unwrap_or(server_routes.len());
    server_routes.insert(insertion, container);
}

fn is_catch_all(route: &Value) -> bool {
    route
        .get("match")
        .and_then(Value::as_array)
        .is_none_or(Vec::is_empty)
}

pub(crate) fn build_proxy_route(
    id: &str,
    hostname: &str,
    upstream: &Url,
    preserve_host: bool,
) -> Value {
    build_proxy_route_for_network(
        id,
        hostname,
        upstream,
        preserve_host,
        "127.0.0.1",
        &["127.0.0.0/8".to_owned(), "::1".to_owned()],
    )
}

pub(crate) fn build_proxy_route_for_network(
    id: &str,
    hostname: &str,
    upstream: &Url,
    preserve_host: bool,
    loopback_host: &str,
    client_ip_ranges: &[String],
) -> Value {
    let host = upstream.host_str().expect("validated upstream has a host");
    let port = upstream
        .port_or_known_default()
        .expect("HTTP(S) has a default port");
    let local = host.eq_ignore_ascii_case("localhost")
        || host
            .parse::<std::net::IpAddr>()
            .is_ok_and(|address| address.is_loopback());
    let dial_host = if local { loopback_host } else { host };
    let default_host = if local && dial_host != host {
        host_port(host, port)
    } else {
        "{http.reverse_proxy.upstream.hostport}".to_owned()
    };
    let mut proxy = json!({
        "handler": "reverse_proxy",
        "upstreams": [{ "dial": host_port(dial_host, port) }],
        "headers": { "request": { "set": {
            "Host": [if preserve_host { "{http.request.host}" } else { &default_host }],
            "X-Forwarded-Host": ["{http.request.host}"]
        } } }
    });
    if upstream.scheme() == "https" {
        let mut tls = json!({});
        if local && dial_host != host {
            tls["server_name"] = json!(host);
        }
        proxy["transport"] = json!({ "protocol": "http", "tls": tls });
    }
    json!({
        "@id": id,
        "match": [{
            "host": [hostname],
            "remote_ip": { "ranges": client_ip_ranges }
        }],
        "handle": [proxy]
    })
}

fn host_port(host: &str, port: u16) -> String {
    if host.contains(':') {
        format!("[{host}]:{port}")
    } else {
        format!("{host}:{port}")
    }
}

pub(crate) fn owner_route_id(owner_id: Uuid) -> String {
    format!("nook_route_v1_{owner_id}")
}

pub(crate) fn parse_owner_route_id(id: &str) -> Option<Uuid> {
    Uuid::parse_str(id.strip_prefix("nook_route_v1_")?).ok()
}

pub(crate) fn update_owned_route(
    server_routes: &mut Vec<Value>,
    container_id: &str,
    owner_id: Uuid,
    replacement: Option<(String, Value)>,
) -> Result<(), Error> {
    let no_tls = container_id == HTTP_CONTAINER_ID;
    let replacement_hostname = replacement.as_ref().map(|(hostname, _)| hostname.as_str());
    let mut managed = Vec::new();
    if let Some(container) = server_routes
        .iter()
        .find(|route| route.get("@id").and_then(Value::as_str) == Some(container_id))
    {
        for route in container
            .pointer("/handle/0/routes")
            .and_then(Value::as_array)
            .ok_or(Error::InvalidOwnedRoute)?
        {
            let id = route
                .get("@id")
                .and_then(Value::as_str)
                .ok_or(Error::InvalidOwnedRoute)?;
            let existing_owner = parse_owner_route_id(id).ok_or(Error::InvalidOwnedRoute)?;
            if existing_owner == owner_id {
                continue;
            }
            let hostname = route
                .pointer("/match/0/host/0")
                .and_then(Value::as_str)
                .ok_or(Error::InvalidOwnedRoute)?
                .to_owned();
            if replacement_hostname == Some(hostname.as_str()) {
                continue;
            }
            managed.push(ManagedRoute {
                hostname,
                no_tls,
                route: route.clone(),
            });
        }
    }
    if let Some((hostname, route)) = replacement {
        managed.push(ManagedRoute {
            hostname,
            no_tls,
            route,
        });
    }
    let (https, http) = build_containers(&managed);
    place_container(
        server_routes,
        container_id,
        if no_tls { http } else { https },
    );
    Ok(())
}

fn reject_managed_hostname_claim(
    server_routes: &[Value],
    container_id: &str,
    hostname: &str,
    owner_id: Uuid,
    replace_existing: bool,
) -> Result<(), Error> {
    let Some(container) = server_routes
        .iter()
        .find(|route| route.get("@id").and_then(Value::as_str) == Some(container_id))
    else {
        return Ok(());
    };
    for route in container
        .pointer("/handle/0/routes")
        .and_then(Value::as_array)
        .ok_or(Error::InvalidOwnedRoute)?
    {
        let existing_hostname = route
            .pointer("/match/0/host/0")
            .and_then(Value::as_str)
            .ok_or(Error::InvalidOwnedRoute)?;
        let existing_owner = route
            .get("@id")
            .and_then(Value::as_str)
            .and_then(parse_owner_route_id)
            .ok_or(Error::InvalidOwnedRoute)?;
        if existing_hostname == hostname && existing_owner != owner_id && !replace_existing {
            return Err(Error::ManagedHostname(hostname.to_owned()));
        }
    }
    Ok(())
}

pub(crate) fn reject_foreign_hostname_claims(
    server_routes: &[Value],
    hostnames: &[String],
) -> Result<(), Error> {
    for route in server_routes {
        if matches!(
            route.get("@id").and_then(Value::as_str),
            Some(HTTPS_CONTAINER_ID | HTTP_CONTAINER_ID)
        ) {
            continue;
        }
        for hostname in hostnames {
            if value_claims_hostname(route, hostname) {
                return Err(Error::ForeignHostname(hostname.clone()));
            }
        }
    }
    Ok(())
}

fn value_claims_hostname(value: &Value, hostname: &str) -> bool {
    match value {
        Value::Object(object) => {
            object
                .get("host")
                .and_then(Value::as_array)
                .is_some_and(|hosts| {
                    hosts
                        .iter()
                        .filter_map(Value::as_str)
                        .any(|host| host.eq_ignore_ascii_case(hostname))
                })
                || object
                    .values()
                    .any(|child| value_claims_hostname(child, hostname))
        }
        Value::Array(values) => values
            .iter()
            .any(|child| value_claims_hostname(child, hostname)),
        _ => false,
    }
}

pub(crate) fn normalize_upstream(target: &str) -> Result<Upstream, Error> {
    if target.bytes().all(|byte| byte.is_ascii_digit()) {
        let port: u16 = target.parse().map_err(|_| Error::InvalidPort)?;
        if port == 0 {
            return Err(Error::InvalidPort);
        }
        return Ok(Upstream {
            url: Url::parse(&format!("http://127.0.0.1:{port}"))
                .expect("generated loopback URL is valid"),
        });
    }
    let url = Url::parse(target).map_err(|error| Error::InvalidUrl(error.to_string()))?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err(Error::UnsupportedScheme);
    }
    if url.host().is_none() {
        return Err(Error::MissingHost);
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(Error::Credentials);
    }
    if url.path() != "/" {
        return Err(Error::Path);
    }
    if url.query().is_some() {
        return Err(Error::Query);
    }
    if url.fragment().is_some() {
        return Err(Error::Fragment);
    }
    Ok(Upstream { url })
}

#[cfg(test)]
mod tests {
    use super::{
        Client, Error, ServerOverrides, ServerSelection, discover_servers, normalize_upstream,
    };
    use serde_json::json;
    #[cfg(unix)]
    use std::fs;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    #[cfg(unix)]
    use std::os::unix::net::UnixListener;

    #[test]
    fn port_becomes_loopback_http_url() {
        assert_eq!(
            normalize_upstream("3000").unwrap().url.as_str(),
            "http://127.0.0.1:3000/"
        );
    }

    #[test]
    fn local_ca_diagnostic_parses_pem_and_builds_the_safe_trust_command() {
        let pem = "-----BEGIN CERTIFICATE-----\nQUJD\nRA==\n-----END CERTIFICATE-----\n";
        assert_eq!(super::pem_certificates(pem), ["QUJDRA=="]);
        assert_eq!(
            super::local_ca_is_trusted("not a certificate"),
            Err(Error::InvalidLocalCa)
        );
        assert_eq!(
            Client::new("http://127.0.0.1:2020")
                .unwrap()
                .trust_command(),
            "caddy trust --address 127.0.0.1:2020"
        );
        #[cfg(unix)]
        {
            assert_eq!(
                Client::new("unix//run/caddy/admin.socket")
                    .unwrap()
                    .trust_command(),
                "caddy trust --address unix//run/caddy/admin.socket"
            );
            assert_eq!(
                Client::new("unix:///run/caddy/admin.socket")
                    .unwrap()
                    .trust_command(),
                "caddy trust --address unix:///run/caddy/admin.socket"
            );
            assert!(matches!(
                Client::new("unix/relative.socket"),
                Err(Error::AdminUrl(_))
            ));
        }
        #[cfg(windows)]
        assert!(matches!(
            Client::new("unix/C:\\caddy\\admin.sock"),
            Err(Error::AdminUrl(reason)) if reason.contains("not supported on Windows")
        ));
    }

    #[test]
    #[cfg(unix)]
    fn admin_client_reads_chunked_config_over_unix_socket() {
        let root = std::env::temp_dir().join(format!(
            "nook-unix-test-{}-{}",
            std::process::id(),
            std::thread::current().name().unwrap_or("unnamed")
        ));
        fs::create_dir_all(&root).unwrap();
        let socket = root.join("admin.socket");
        let listener = UnixListener::bind(&socket).unwrap();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = Vec::new();
            while !request.windows(4).any(|window| window == b"\r\n\r\n") {
                let mut chunk = [0; 256];
                let size = stream.read(&mut chunk).unwrap();
                assert_ne!(size, 0, "request ended before its headers were complete");
                request.extend_from_slice(&chunk[..size]);
            }
            assert!(String::from_utf8_lossy(&request).starts_with("GET /config/ "));
            stream
                .write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\nf\r\n{\"apps\":{\"http\"\r\n11\r\n:{\"servers\":{}}}}\r\n0\r\n\r\n",
                )
                .unwrap();
        });
        let config = Client::new(&format!("unix/{}", socket.display()))
            .unwrap()
            .fetch_config()
            .unwrap();
        assert!(config.pointer("/apps/http/servers").is_some());
        server.join().unwrap();
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    #[cfg(unix)]
    fn unix_permission_error_explains_persistent_caddy_listener_mode() {
        let error = super::unix_connect_error(
            std::path::Path::new("/run/caddy/admin.socket"),
            std::io::Error::from(std::io::ErrorKind::PermissionDenied),
        );
        let message = error.to_string();
        assert!(message.contains("unix//run/caddy/admin.socket|0660"));
        assert!(message.contains("ExecStartPost chmod alone is not persistent"));
    }

    #[test]
    fn accepts_http_and_https_hosts_on_any_network() {
        for target in [
            "http://127.0.0.1:8080",
            "http://192.168.1.2",
            "https://example.com",
        ] {
            assert!(normalize_upstream(target).is_ok(), "{target}");
        }
    }

    #[test]
    fn rejects_each_forbidden_url_component_precisely() {
        assert_eq!(
            normalize_upstream("https://user@example.com"),
            Err(Error::Credentials)
        );
        assert_eq!(
            normalize_upstream("https://example.com/api"),
            Err(Error::Path)
        );
        assert_eq!(
            normalize_upstream("https://example.com/?x=1"),
            Err(Error::Query)
        );
        assert_eq!(
            normalize_upstream("https://example.com/#top"),
            Err(Error::Fragment)
        );
        assert_eq!(
            normalize_upstream("ftp://example.com"),
            Err(Error::UnsupportedScheme)
        );
        assert_eq!(normalize_upstream("0"), Err(Error::InvalidPort));
        assert_eq!(normalize_upstream("70000"), Err(Error::InvalidPort));
    }

    #[test]
    fn discovers_unique_https_and_explicit_http_servers() {
        let config = json!({"apps":{"http":{"servers":{
            "secure":{"listen":[":443"]},
            "plain":{"listen":["tcp/:80"]},
            "other":{"listen":[":8080"]}
        }}}});
        assert_eq!(
            discover_servers(&config, ServerOverrides::default(), true, true).unwrap(),
            ServerSelection {
                https: Some("secure".into()),
                http: Some("plain".into())
            }
        );
    }

    #[test]
    fn ambiguity_lists_candidates_and_override_resolves_it() {
        let config = json!({"apps":{"http":{"servers":{
            "a":{"listen":[":443"]},
            "b":{"listen":["0.0.0.0:443"]}
        }}}});
        assert!(matches!(
            discover_servers(&config, ServerOverrides::default(), true, false),
            Err(Error::AmbiguousServer { candidates, .. }) if candidates == ["a", "b"]
        ));
        assert_eq!(
            discover_servers(
                &config,
                ServerOverrides {
                    https: Some("b"),
                    http: None
                },
                true,
                false
            )
            .unwrap()
            .https
            .as_deref(),
            Some("b")
        );
    }

    #[test]
    fn available_server_discovery_allows_absent_http_but_rejects_ambiguity() {
        let config = json!({"apps":{"http":{"servers":{
            "https":{"listen":[":443"],"routes":[]}
        }}}});
        assert_eq!(
            super::discover_available_servers(&config, ServerOverrides::default()).unwrap(),
            ServerSelection {
                https: Some("https".into()),
                http: None
            }
        );
        let ambiguous = json!({"apps":{"http":{"servers":{
            "one":{"listen":[":443"]},
            "two":{"listen":["127.0.0.1:443"]}
        }}}});
        assert!(matches!(
            super::discover_available_servers(&ambiguous, ServerOverrides::default()),
            Err(Error::AmbiguousServer { kind: "HTTPS", .. })
        ));
    }

    #[test]
    fn managed_inspection_reports_containers_and_owned_routes() {
        let owner = uuid::Uuid::new_v4();
        let upstream = url::Url::parse("http://127.0.0.1:3000").unwrap();
        let route = super::build_proxy_route(
            &super::owner_route_id(owner),
            "api.localhost",
            &upstream,
            false,
        );
        let (container, _) = super::build_containers(&[super::ManagedRoute {
            hostname: "api.localhost".into(),
            no_tls: false,
            route,
        }]);
        let config = json!({"apps":{"http":{"servers":{
            "https":{"listen":[":443"],"routes":[container.unwrap()]}
        }}}});
        let inspection = super::inspect_managed(
            &config,
            &ServerSelection {
                https: Some("https".into()),
                http: None,
            },
        )
        .unwrap();
        assert!(inspection.https_container);
        assert!(!inspection.http_container);
        assert_eq!(inspection.routes.len(), 1);
        assert_eq!(inspection.routes[0].owner_id, owner);
        assert_eq!(inspection.routes[0].hostname, "api.localhost");
    }

    #[test]
    fn no_tls_requires_an_explicit_port_80_server() {
        let config = json!({"apps":{"http":{"servers":{"secure":{"listen":[":443"]}}}}});
        assert!(matches!(
            discover_servers(&config, ServerOverrides::default(), false, true),
            Err(Error::AmbiguousServer { kind: "HTTP", candidates, .. }) if candidates.is_empty()
        ));
    }

    #[test]
    fn missing_server_diagnostic_requests_a_listener_instead_of_an_override() {
        let config = json!({"apps":{"http":{"servers":{}}}});
        let error = discover_servers(&config, ServerOverrides::default(), true, false)
            .unwrap_err()
            .to_string();
        assert!(error.contains("listening explicitly on :443"));
        assert!(!error.contains("override"));
    }

    #[test]
    fn admin_client_reads_config_without_caddy_executable() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0; 1024];
            let size = stream.read(&mut request).unwrap();
            assert!(String::from_utf8_lossy(&request[..size]).starts_with("GET /config/ "));
            let body = r#"{"apps":{"http":{"servers":{}}}}"#;
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            )
            .unwrap();
        });
        let config = Client::new(&format!("http://{address}"))
            .unwrap()
            .fetch_config()
            .unwrap();
        assert!(config.pointer("/apps/http/servers").is_some());
        server.join().unwrap();
    }

    #[test]
    fn containers_partition_tls_and_keep_https_hosts_top_level() {
        let routes = vec![
            super::ManagedRoute {
                hostname: "b.localhost".into(),
                no_tls: false,
                route: json!({"marker":"https-b"}),
            },
            super::ManagedRoute {
                hostname: "a.localhost".into(),
                no_tls: false,
                route: json!({"marker":"https-a"}),
            },
            super::ManagedRoute {
                hostname: "plain.localhost".into(),
                no_tls: true,
                route: json!({"marker":"http"}),
            },
        ];
        let (https, http) = super::build_containers(&routes);
        let https = https.unwrap();
        assert_eq!(
            https.pointer("/match/0/host").unwrap(),
            &json!(["a.localhost", "b.localhost"])
        );
        assert_eq!(
            https
                .pointer("/handle/0/routes")
                .unwrap()
                .as_array()
                .unwrap()
                .len(),
            2
        );
        let http = http.unwrap();
        assert_eq!(
            http.pointer("/match/0/host").unwrap(),
            &json!(["plain.localhost"])
        );
        assert_eq!(http.pointer("/handle/0/routes/0/marker").unwrap(), "http");
    }

    #[test]
    fn container_is_repositioned_before_first_catch_all() {
        let old = json!({"@id": super::HTTPS_CONTAINER_ID, "match":[{"host":["old.localhost"]}]});
        let foreign = json!({"match":[{"host":["foreign.localhost"]}], "marker":"foreign"});
        let catch_all = json!({"handle":[], "marker":"catch-all"});
        let container =
            json!({"@id": super::HTTPS_CONTAINER_ID, "match":[{"host":["new.localhost"]}]});
        let mut routes = vec![foreign.clone(), catch_all.clone(), old];
        super::place_container(
            &mut routes,
            super::HTTPS_CONTAINER_ID,
            Some(container.clone()),
        );
        assert_eq!(routes, vec![foreign, container, catch_all]);
    }

    #[test]
    fn empty_container_is_removed_without_touching_foreign_routes() {
        let foreign = json!({"marker":"foreign"});
        let mut routes = vec![json!({"@id": super::HTTP_CONTAINER_ID}), foreign.clone()];
        super::place_container(&mut routes, super::HTTP_CONTAINER_ID, None);
        assert_eq!(routes, vec![foreign]);
    }

    #[test]
    fn proxy_route_combines_host_and_loopback_in_one_matcher() {
        let upstream = url::Url::parse("http://127.0.0.1:3000").unwrap();
        let route = super::build_proxy_route("nook_route_1", "api.localhost", &upstream, false);
        assert_eq!(
            route.pointer("/match/0/host").unwrap(),
            &json!(["api.localhost"])
        );
        assert_eq!(
            route.pointer("/match/0/remote_ip/ranges").unwrap(),
            &json!(["127.0.0.0/8", "::1"])
        );
        assert_eq!(
            route.pointer("/handle/0/headers/request/set/Host/0"),
            Some(&json!("{http.reverse_proxy.upstream.hostport}"))
        );
        assert_eq!(
            route.pointer("/handle/0/headers/request/set/X-Forwarded-Host/0"),
            Some(&json!("{http.request.host}"))
        );
    }

    #[test]
    fn preserve_host_uses_the_requested_localhost_domain() {
        let upstream = url::Url::parse("https://service.example:8443").unwrap();
        let route = super::build_proxy_route("nook_route_1", "api.localhost", &upstream, true);
        assert_eq!(
            route.pointer("/handle/0/headers/request/set/Host/0"),
            Some(&json!("{http.request.host}"))
        );
        assert_eq!(
            route.pointer("/handle/0/headers/request/set/X-Forwarded-Host/0"),
            Some(&json!("{http.request.host}"))
        );
    }

    #[test]
    fn docker_route_translates_only_the_connection_address() {
        let upstream = url::Url::parse("https://127.0.0.1:8443").unwrap();
        let route = super::build_proxy_route_for_network(
            "nook_route_1",
            "api.localhost",
            &upstream,
            false,
            "host.docker.internal",
            &["172.30.0.1/32".into()],
        );
        assert_eq!(
            route.pointer("/handle/0/upstreams/0/dial"),
            Some(&json!("host.docker.internal:8443"))
        );
        assert_eq!(
            route.pointer("/handle/0/headers/request/set/Host/0"),
            Some(&json!("127.0.0.1:8443"))
        );
        assert_eq!(
            route.pointer("/handle/0/transport/tls/server_name"),
            Some(&json!("127.0.0.1"))
        );
        assert_eq!(
            route.pointer("/match/0/remote_ip/ranges"),
            Some(&json!(["172.30.0.1/32"]))
        );
    }

    #[test]
    fn https_proxy_enables_tls_without_disabling_verification() {
        let upstream = url::Url::parse("https://example.com").unwrap();
        let route = super::build_proxy_route("nook_route_1", "api.localhost", &upstream, false);
        assert_eq!(
            route.pointer("/handle/0/transport/tls").unwrap(),
            &json!({})
        );
        assert!(route.to_string().find("insecure").is_none());
    }

    #[test]
    fn foreign_claim_is_rejected_but_nook_container_is_ignored() {
        let routes = vec![
            json!({"@id": super::HTTPS_CONTAINER_ID, "match":[{"host":["owned.localhost"]}]}),
            json!({"handle":[{"handler":"subroute","routes":[{"match":[{"host":["foreign.localhost"]}]}]}]}),
        ];
        assert!(
            super::reject_foreign_hostname_claims(&routes, &["owned.localhost".into()]).is_ok()
        );
        assert_eq!(
            super::reject_foreign_hostname_claims(&routes, &["foreign.localhost".into()]),
            Err(Error::ForeignHostname("foreign.localhost".into()))
        );
    }

    #[test]
    fn retries_re_read_reapply_and_use_exact_bounded_delays() {
        use std::cell::RefCell;
        use std::time::Duration;

        let versions = RefCell::new(
            vec![
                ("v1".to_owned(), vec![json!({"foreign":1})]),
                (
                    "v2".to_owned(),
                    vec![json!({"foreign":1}), json!({"foreign":2})],
                ),
                (
                    "v3".to_owned(),
                    vec![json!({"foreign":1}), json!({"foreign":2})],
                ),
                (
                    "v4".to_owned(),
                    vec![json!({"foreign":1}), json!({"foreign":2})],
                ),
            ]
            .into_iter(),
        );
        let writes = RefCell::new(0);
        let delays = RefCell::new(Vec::new());
        super::mutate_with_retry(
            || Ok(versions.borrow_mut().next().unwrap()),
            |_etag, routes| {
                assert!(
                    routes
                        .iter()
                        .any(|route| route.get("@id").and_then(serde_json::Value::as_str)
                            == Some("nook_https_routes_v1"))
                );
                let mut writes = writes.borrow_mut();
                *writes += 1;
                Ok(if *writes < 4 {
                    super::WriteOutcome::PreconditionFailed
                } else {
                    super::WriteOutcome::Applied
                })
            },
            |mut routes| {
                super::place_container(
                    &mut routes,
                    super::HTTPS_CONTAINER_ID,
                    Some(json!({"@id":super::HTTPS_CONTAINER_ID})),
                );
                Ok(routes)
            },
            |delay| delays.borrow_mut().push(delay),
        )
        .unwrap();
        assert_eq!(
            *delays.borrow(),
            [
                Duration::from_millis(25),
                Duration::from_millis(50),
                Duration::from_millis(100)
            ]
        );
    }

    #[test]
    fn fourth_precondition_failure_is_actionable_error() {
        let mut delays = Vec::new();
        let result = super::mutate_with_retry(
            || Ok(("etag".into(), Vec::new())),
            |_, _| Ok(super::WriteOutcome::PreconditionFailed),
            Ok,
            |delay| delays.push(delay),
        );
        assert_eq!(result, Err(Error::ConcurrentMutation));
        assert_eq!(delays.len(), 3);
    }

    #[test]
    fn owner_marker_round_trips_and_rejects_foreign_ids() {
        let owner = uuid::Uuid::new_v4();
        let marker = super::owner_route_id(owner);
        assert_eq!(super::parse_owner_route_id(&marker), Some(owner));
        assert_eq!(super::parse_owner_route_id("foreign_route"), None);
    }

    #[test]
    fn stale_owner_cleanup_cannot_remove_replacement_route() {
        let old_owner = uuid::Uuid::new_v4();
        let new_owner = uuid::Uuid::new_v4();
        let upstream = url::Url::parse("http://127.0.0.1:3000").unwrap();
        let new_route = super::build_proxy_route(
            &super::owner_route_id(new_owner),
            "api.localhost",
            &upstream,
            false,
        );
        let managed = super::ManagedRoute {
            hostname: "api.localhost".into(),
            no_tls: false,
            route: new_route.clone(),
        };
        let (container, _) = super::build_containers(&[managed]);
        let mut routes = vec![container.unwrap()];
        super::update_owned_route(&mut routes, super::HTTPS_CONTAINER_ID, old_owner, None).unwrap();
        assert_eq!(routes[0].pointer("/handle/0/routes/0").unwrap(), &new_route);
    }

    #[test]
    fn managed_hostname_replacement_requires_explicit_force() {
        let old_owner = uuid::Uuid::new_v4();
        let new_owner = uuid::Uuid::new_v4();
        let upstream = url::Url::parse("http://127.0.0.1:3000").unwrap();
        let old_route = super::build_proxy_route(
            &super::owner_route_id(old_owner),
            "api.localhost",
            &upstream,
            false,
        );
        let (container, _) = super::build_containers(&[super::ManagedRoute {
            hostname: "api.localhost".into(),
            no_tls: false,
            route: old_route,
        }]);
        let routes = vec![container.unwrap()];
        assert_eq!(
            super::reject_managed_hostname_claim(
                &routes,
                super::HTTPS_CONTAINER_ID,
                "api.localhost",
                new_owner,
                false,
            ),
            Err(Error::ManagedHostname("api.localhost".into()))
        );
        assert!(
            super::reject_managed_hostname_claim(
                &routes,
                super::HTTPS_CONTAINER_ID,
                "api.localhost",
                new_owner,
                true,
            )
            .is_ok()
        );
    }

    #[test]
    fn owned_route_upsert_and_removal_are_idempotent() {
        let owner = uuid::Uuid::new_v4();
        let upstream = url::Url::parse("http://127.0.0.1:3000").unwrap();
        let route = super::build_proxy_route(
            &super::owner_route_id(owner),
            "api.localhost",
            &upstream,
            false,
        );
        let mut routes = Vec::new();
        for _ in 0..2 {
            super::update_owned_route(
                &mut routes,
                super::HTTPS_CONTAINER_ID,
                owner,
                Some(("api.localhost".into(), route.clone())),
            )
            .unwrap();
        }
        assert_eq!(
            routes[0]
                .pointer("/handle/0/routes")
                .unwrap()
                .as_array()
                .unwrap()
                .len(),
            1
        );
        for _ in 0..2 {
            super::update_owned_route(&mut routes, super::HTTPS_CONTAINER_ID, owner, None).unwrap();
        }
        assert!(routes.is_empty());
    }

    #[test]
    fn managed_backend_applies_owned_route_with_etag() {
        use crate::reconcile::{RouteBackend, RouteSpec};
        use crate::state::Scheme;

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let owner = uuid::Uuid::new_v4();
        let expected_marker = super::owner_route_id(owner);
        let server = std::thread::spawn(move || {
            for step in 0..2 {
                let (mut stream, _) = listener.accept().unwrap();
                let request = read_http_request(&mut stream);
                let request = String::from_utf8_lossy(&request);
                if step == 0 {
                    assert!(request.starts_with("GET /config/apps/http/servers/https/routes "));
                    write!(
                        stream,
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nETag: \"v1\"\r\nContent-Length: 2\r\nConnection: close\r\n\r\n[]"
                    )
                    .unwrap();
                } else {
                    assert!(request.starts_with("PATCH /config/apps/http/servers/https/routes "));
                    assert!(request.to_ascii_lowercase().contains("if-match: \"v1\""));
                    assert!(request.contains(&expected_marker));
                    write!(
                        stream,
                        "HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                    )
                    .unwrap();
                }
            }
        });
        let client = Client::new(&format!("http://{address}")).unwrap();
        let mut backend = super::ManagedCaddyRoutes {
            client: &client,
            https_server: Some("https"),
            http_server: None,
            loopback_host: "127.0.0.1",
            client_ip_ranges: &["127.0.0.0/8".into(), "::1".into()],
        };
        backend
            .ensure(&RouteSpec {
                owner_id: owner,
                hostname: "api.localhost".into(),
                target: "http://127.0.0.1:3000".into(),
                scheme: Scheme::Http,
                tls: true,
                replace_existing: false,
                preserve_host: false,
            })
            .unwrap();
        server.join().unwrap();
    }

    fn read_http_request(stream: &mut std::net::TcpStream) -> Vec<u8> {
        let mut request = Vec::new();
        loop {
            let mut chunk = [0; 4096];
            let size = stream.read(&mut chunk).unwrap();
            request.extend_from_slice(&chunk[..size]);
            let Some(headers_end) = request.windows(4).position(|window| window == b"\r\n\r\n")
            else {
                continue;
            };
            let headers_end = headers_end + 4;
            let headers = String::from_utf8_lossy(&request[..headers_end]);
            let content_length = headers
                .lines()
                .find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    name.eq_ignore_ascii_case("content-length")
                        .then(|| value.trim().parse::<usize>().ok())
                        .flatten()
                })
                .unwrap_or(0);
            if request.len() >= headers_end + content_length {
                return request;
            }
        }
    }
}
