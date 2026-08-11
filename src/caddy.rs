//! Caddy Admin API integration and canonical proxy targets.
#![allow(dead_code)]

use serde_json::Value;
use serde_json::json;
use std::fmt;
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
pub(crate) struct Client {
    admin: Url,
}

impl Client {
    pub(crate) fn new(admin: &str) -> Result<Self, Error> {
        let admin = Url::parse(admin).map_err(|error| Error::AdminUrl(error.to_string()))?;
        if !matches!(admin.scheme(), "http" | "https") || admin.host().is_none() {
            return Err(Error::AdminUrl("expected an absolute HTTP(S) URL".into()));
        }
        Ok(Self { admin })
    }

    pub(crate) fn fetch_config(&self) -> Result<Value, Error> {
        let endpoint = self
            .admin
            .join("/config/")
            .map_err(|error| Error::AdminUrl(error.to_string()))?;
        let mut response = ureq::get(endpoint.as_str())
            .call()
            .map_err(|error| Error::AdminRequest(error.to_string()))?;
        response
            .body_mut()
            .read_json()
            .map_err(|error| Error::AdminRequest(error.to_string()))
    }

    pub(crate) fn mutate_server_routes(
        &self,
        server: &str,
        transform: impl FnMut(Vec<Value>) -> Result<Vec<Value>, Error>,
    ) -> Result<(), Error> {
        let endpoint = self.server_routes_endpoint(server)?;
        mutate_with_retry(
            || {
                let mut response = ureq::get(endpoint.as_str()).call().map_err(admin_request)?;
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
            |etag, routes| match ureq::put(endpoint.as_str())
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

    fn server_routes_endpoint(&self, server: &str) -> Result<Url, Error> {
        let mut endpoint = self.admin.clone();
        endpoint.set_path("");
        endpoint
            .path_segments_mut()
            .map_err(|()| Error::AdminUrl("URL cannot be a base".into()))?
            .extend(["config", "apps", "http", "servers", server, "routes"]);
        Ok(endpoint)
    }
}

pub(crate) struct ManagedCaddyRoutes<'a> {
    pub(crate) client: &'a Client,
    pub(crate) https_server: Option<&'a str>,
    pub(crate) http_server: Option<&'a str>,
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
        let proxy = build_proxy_route(
            &owner_route_id(owner_id),
            &hostname,
            &upstream.url,
            route.preserve_host,
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
    let host = upstream.host_str().expect("validated upstream has a host");
    let port = upstream
        .port_or_known_default()
        .expect("HTTP(S) has a default port");
    let mut proxy = json!({
        "handler": "reverse_proxy",
        "upstreams": [{ "dial": format!("{host}:{port}") }],
        "headers": { "request": { "set": {
            "Host": [if preserve_host { "{http.request.host}" } else { "{http.reverse_proxy.upstream.hostport}" }],
            "X-Forwarded-Host": ["{http.request.host}"]
        } } }
    });
    if upstream.scheme() == "https" {
        proxy["transport"] = json!({ "protocol": "http", "tls": {} });
    }
    json!({
        "@id": id,
        "match": [{
            "host": [hostname],
            "remote_ip": { "ranges": ["127.0.0.0/8", "::1"] }
        }],
        "handle": [proxy]
    })
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
    use std::io::{Read, Write};
    use std::net::TcpListener;

    #[test]
    fn port_becomes_loopback_http_url() {
        assert_eq!(
            normalize_upstream("3000").unwrap().url.as_str(),
            "http://127.0.0.1:3000/"
        );
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
                    assert!(request.starts_with("PUT /config/apps/http/servers/https/routes "));
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
