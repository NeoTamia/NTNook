//! Caddy Admin API integration and canonical proxy targets.
#![allow(dead_code)]

use serde_json::Value;
use serde_json::json;
use std::fmt;
use url::Url;

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
                write!(formatter, "Caddy Admin API request failed: {reason}")
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

pub(crate) fn discover_servers(
    config: &Value,
    overrides: ServerOverrides<'_>,
    need_https: bool,
    need_http: bool,
) -> Result<ServerSelection, Error> {
    let servers = config
        .pointer("/apps/http/servers")
        .and_then(Value::as_object)
        .ok_or(Error::InvalidConfig("apps.http.servers is missing"))?;
    let candidates = |port| {
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
        names
    };
    let https_candidates = candidates(443);
    let http_candidates = candidates(80);
    Ok(ServerSelection {
        https: need_https
            .then(|| select_server("HTTPS", overrides.https, &https_candidates))
            .transpose()?,
        http: need_http
            .then(|| select_server("HTTP", overrides.http, &http_candidates))
            .transpose()?,
    })
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
}
