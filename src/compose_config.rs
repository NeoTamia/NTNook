//! Read canonical JSON; Docker owns YAML, interpolation and merging.
use crate::{
    compose::invalid,
    state::{Alias, Scheme},
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use uuid::Uuid;

pub(crate) const PREFIX: &str = "io.ntnook.route.";
pub(crate) const OWNER: &str = "io.ntnook.owner";
pub(crate) const IDENTITY: &str = "io.ntnook.identity";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Endpoint {
    pub(crate) service: String,
    pub(crate) port: u16,
    pub(crate) alias: Alias,
    pub(crate) active: bool,
    pub(crate) desired: bool,
    pub(crate) status: String,
}

pub(crate) fn endpoints(
    model: &Value,
    namespace: &str,
    worktree: Option<&crate::worktree::Identity>,
) -> crate::Result<BTreeMap<String, Endpoint>> {
    let services = model["services"]
        .as_object()
        .ok_or_else(|| invalid("Compose model has no services"))?;
    let mut result = BTreeMap::new();
    let mut names = BTreeSet::new();
    for kind in ["networks", "volumes"] {
        if let Some(resources) = model[kind].as_object() {
            for (key, resource) in resources {
                // Canonical JSON contains generated names too; require project scoping.
                let expected = format!("{}_{}", model["name"].as_str().unwrap_or_default(), key);
                if resource["external"] != true
                    && resource["name"]
                        .as_str()
                        .is_some_and(|name| name != expected)
                {
                    return Err(invalid(format!(
                        "{kind}.{key}: shared names require external: true"
                    ))
                    .into());
                }
            }
        }
    }
    for (service, config) in services {
        if config.get("container_name").is_some() {
            return Err(invalid(format!(
                "{service}: container_name defeats project isolation"
            ))
            .into());
        }
        if ["pre_start", "post_start", "pre_stop", "provider"]
            .iter()
            .any(|key| config.get(key).is_some())
        {
            return Err(invalid(format!(
                "{service}: lifecycle hooks/providers are not supported"
            ))
            .into());
        }
        let mut groups: BTreeMap<String, BTreeMap<String, String>> = BTreeMap::new();
        if let Some(labels) = config["labels"].as_object() {
            if labels.contains_key(OWNER) || labels.contains_key(IDENTITY) {
                return Err(
                    invalid(format!("{service}: Nook ownership labels are reserved")).into(),
                );
            }
            for (key, value) in labels {
                if let Some(rest) = key.strip_prefix(PREFIX) {
                    let (id, field) = rest
                        .split_once('.')
                        .ok_or_else(|| invalid(format!("invalid route label {key}")))?;
                    crate::worktree::validate_label(id)?;
                    if !["port", "name", "tls", "preserve-host"].contains(&field) {
                        return Err(invalid(format!("unknown route field {key}")).into());
                    }
                    groups.entry(id.into()).or_default().insert(
                        field.into(),
                        value
                            .as_str()
                            .ok_or_else(|| invalid("route labels must be strings"))?
                            .into(),
                    );
                }
            }
        }
        if groups.is_empty() {
            continue;
        }
        if config.get("network_mode").is_some()
            || config["scale"].as_u64().is_some_and(|n| n != 1)
            || config["deploy"]["replicas"]
                .as_u64()
                .is_some_and(|n| n != 1)
        {
            return Err(invalid(format!(
                "{service}: routed services require one replica and ordinary Compose networking"
            ))
            .into());
        }
        for (id, fields) in groups {
            let port = fields
                .get("port")
                .and_then(|p| p.parse::<u16>().ok())
                .filter(|p| *p > 0)
                .ok_or_else(|| {
                    invalid(format!(
                        "{service}/{id}: a valid internal HTTP port is required"
                    ))
                })?;
            let ports: Vec<_> = config["ports"]
                .as_array()
                .into_iter()
                .flatten()
                .filter(|p| p["target"].as_u64() == Some(u64::from(port)))
                .collect();
            if ports.is_empty() {
                return Err(invalid(format!(
                    "{service}/{id}: HTTP port {port} must be published in Compose ports"
                ))
                .into());
            }
            for mapping in ports {
                if mapping["protocol"].as_str().unwrap_or("tcp") != "tcp"
                    || mapping["app_protocol"]
                        .as_str()
                        .is_some_and(|p| p != "http")
                {
                    return Err(invalid(format!(
                        "{service}/{id}: only HTTP over TCP is supported"
                    ))
                    .into());
                }
                if mapping["published"]
                    .as_str()
                    .is_some_and(|p| p.contains('-'))
                {
                    return Err(invalid("published port ranges are not supported; omit published for dynamic allocation").into());
                }
            }
            let name = fields
                .get("name")
                .cloned()
                .unwrap_or_else(|| format!("{service}-{id}"));
            let name = crate::config::normalize_hostname(&name)?;
            let hostname = crate::config::normalize_hostname(&format!(
                "{}.{}",
                name.trim_end_matches(".localhost"),
                namespace
            ))?;
            let hostname = match worktree {
                Some(w) => crate::worktree::hostname(&hostname, w, Some(namespace))?,
                None => hostname,
            };
            if !names.insert(hostname.clone()) {
                return Err(invalid(format!("duplicate Compose hostname {hostname}")).into());
            }
            let boolean = |field: &str| -> crate::Result<bool> {
                match fields.get(field).map(String::as_str) {
                    None | Some("true") => Ok(true),
                    Some("false") => Ok(false),
                    _ => Err(
                        invalid(format!("{service}/{id}: {field} must be true or false")).into(),
                    ),
                }
            };
            result.insert(
                format!("{service}/{id}"),
                Endpoint {
                    service: service.clone(),
                    port,
                    active: false,
                    desired: true,
                    status: "starting".into(),
                    alias: Alias {
                        id: Uuid::new_v4(),
                        hostname,
                        target: String::new(),
                        scheme: Scheme::Http,
                        tls: boolean("tls")?,
                        preserve_host: boolean("preserve-host")?,
                    },
                },
            );
        }
    }
    Ok(result)
}

/// Collapse equivalent IPv4/IPv6 wildcard publications, never choose an arbitrary port.
pub(crate) fn target(container: &Value, port: u16) -> crate::Result<Option<String>> {
    let key = format!("{port}/tcp");
    let Some(bindings) = container["NetworkSettings"]["Ports"][&key].as_array() else {
        return Ok(None);
    };
    let mut targets = BTreeSet::new();
    for binding in bindings {
        let ip: std::net::IpAddr = binding["HostIp"]
            .as_str()
            .ok_or_else(|| invalid("missing published host IP"))?
            .parse()
            .map_err(|_| invalid("invalid published host IP"))?;
        let host_port: u16 = binding["HostPort"]
            .as_str()
            .and_then(|p| p.parse().ok())
            .filter(|p| *p > 0)
            .ok_or_else(|| invalid("invalid published host port"))?;
        let ip = if ip.is_unspecified() {
            "127.0.0.1".parse().unwrap()
        } else {
            ip
        };
        targets.insert(format!(
            "http://{}/",
            std::net::SocketAddr::new(ip, host_port)
        ));
    }
    if targets.len() > 1 {
        return Err(invalid(format!("port {port} has ambiguous publications")).into());
    }
    Ok(targets.into_iter().next())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn model() -> Value {
        json!({"name":"app", "services":{"web":{"ports":[{"target":3000,"protocol":"tcp"}],"labels":{"io.ntnook.route.web.port":"3000"}},"db":{"expose":[5432]}}})
    }
    #[test]
    fn only_explicit_http_is_routed() {
        let routes = endpoints(&model(), "app", None).unwrap();
        assert_eq!(routes.len(), 1);
        assert_eq!(routes["web/web"].alias.hostname, "web-web.app.localhost");
    }
    #[test]
    fn rejects_udp_unpublished_and_scaled_services() {
        for update in [
            json!({"ports":[{"target":3000,"protocol":"udp"}]}),
            json!({"ports":[]}),
            json!({"scale":2}),
            json!({"network_mode":"host"}),
            json!({"container_name":"shared"}),
        ] {
            let mut m = model();
            for (k, v) in update.as_object().unwrap() {
                m["services"]["web"][k] = v.clone();
            }
            assert!(endpoints(&m, "app", None).is_err());
        }
    }
    #[test]
    fn concrete_publications_and_ambiguity() {
        let mut c = json!({"NetworkSettings":{"Ports":{"3000/tcp":[{"HostIp":"0.0.0.0","HostPort":"49152"},{"HostIp":"::","HostPort":"49152"}]}}});
        assert_eq!(
            target(&c, 3000).unwrap().unwrap(),
            "http://127.0.0.1:49152/"
        );
        c["NetworkSettings"]["Ports"]["3000/tcp"][1]["HostPort"] = json!("49153");
        assert!(target(&c, 3000).is_err());
    }
}
