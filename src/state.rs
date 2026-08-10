//! Versioned persistent aliases, run leases, and recovery operations.
#![allow(dead_code)]

use std::collections::BTreeMap;
use std::env;
use std::ffi::OsString;
use std::fmt;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

const FORMAT_VERSION: u32 = 1;

#[derive(Debug)]
pub(crate) enum Error {
    MissingHome,
    InvalidJson(serde_json::Error),
    MissingVersion,
    UnsupportedVersion(u64),
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingHome => write!(formatter, "cannot locate the state directory"),
            Self::InvalidJson(error) => write!(formatter, "invalid state registry: {error}"),
            Self::MissingVersion => {
                write!(formatter, "state registry has no numeric format_version")
            }
            Self::UnsupportedVersion(version) => {
                write!(formatter, "unsupported state format_version {version}")
            }
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::InvalidJson(error) => Some(error),
            _ => None,
        }
    }
}

#[derive(Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Registry {
    pub(crate) format_version: u32,
    #[serde(default)]
    pub(crate) aliases: BTreeMap<String, Alias>,
    #[serde(default)]
    pub(crate) leases: BTreeMap<Uuid, Lease>,
    #[serde(default)]
    pub(crate) selected_servers: SelectedServers,
    pub(crate) last_synchronized_at_unix_ms: Option<u64>,
    #[serde(default)]
    pub(crate) pending_operations: Vec<PendingOperation>,
}

impl Registry {
    pub(crate) fn empty() -> Self {
        Self {
            format_version: FORMAT_VERSION,
            ..Self::default()
        }
    }
}

#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Alias {
    pub(crate) hostname: String,
    pub(crate) target: String,
    pub(crate) scheme: Scheme,
    pub(crate) tls: bool,
    pub(crate) preserve_host: bool,
}

#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Lease {
    pub(crate) id: Uuid,
    pub(crate) hostname: String,
    pub(crate) target: String,
    pub(crate) scheme: Scheme,
    pub(crate) pid: u32,
    pub(crate) pgid: i32,
    pub(crate) process_start_time_ticks: u64,
    pub(crate) state: LeaseState,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Scheme {
    Http,
    Https,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum LeaseState {
    Starting,
    Ready,
}

#[derive(Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SelectedServers {
    pub(crate) https: Option<String>,
    pub(crate) http: Option<String>,
}

#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PendingOperation {
    pub(crate) id: Uuid,
    pub(crate) kind: PendingOperationKind,
}

#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum PendingOperationKind {
    InstallRoute { hostname: String, owner_id: Uuid },
    RemoveRoute { hostname: String, owner_id: Uuid },
    StartProcess { lease_id: Uuid },
    FinalizeLease { lease_id: Uuid },
}

pub(crate) fn decode(contents: &[u8]) -> Result<Registry, Error> {
    let value: Value = serde_json::from_slice(contents).map_err(Error::InvalidJson)?;
    let version = value
        .get("format_version")
        .and_then(Value::as_u64)
        .ok_or(Error::MissingVersion)?;
    match version {
        1 => serde_json::from_value(value).map_err(Error::InvalidJson),
        other => Err(Error::UnsupportedVersion(other)),
    }
}

pub(crate) fn state_path() -> Result<PathBuf, Error> {
    state_path_with(|key| env::var_os(key))
}

fn state_path_with(get: impl Fn(&str) -> Option<OsString>) -> Result<PathBuf, Error> {
    if let Some(directory) = get("XDG_STATE_HOME").filter(|value| !value.is_empty()) {
        return Ok(PathBuf::from(directory).join("nook/state.json"));
    }
    get("HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .map(|home| home.join(".local/state/nook/state.json"))
        .ok_or(Error::MissingHome)
}

#[cfg(test)]
mod tests {
    use super::{
        Error, LeaseState, PendingOperation, PendingOperationKind, Registry, decode,
        state_path_with,
    };
    use std::ffi::OsString;
    use std::path::Path;
    use uuid::Uuid;

    #[test]
    fn state_path_prefers_xdg_and_falls_back_to_home() {
        let xdg =
            state_path_with(|key| (key == "XDG_STATE_HOME").then(|| OsString::from("/state")))
                .unwrap();
        assert_eq!(xdg, Path::new("/state/nook/state.json"));
        let home =
            state_path_with(|key| (key == "HOME").then(|| OsString::from("/home/user"))).unwrap();
        assert_eq!(home, Path::new("/home/user/.local/state/nook/state.json"));
    }

    #[test]
    fn registry_round_trip_preserves_recovery_operations_without_argv() {
        let mut registry = Registry::empty();
        let owner = Uuid::new_v4();
        registry.pending_operations.push(PendingOperation {
            id: Uuid::new_v4(),
            kind: PendingOperationKind::RemoveRoute {
                hostname: "api.localhost".into(),
                owner_id: owner,
            },
        });
        let json = serde_json::to_vec(&registry).unwrap();
        assert!(!String::from_utf8_lossy(&json).contains("argv"));
        assert_eq!(decode(&json).unwrap(), registry);
    }

    #[test]
    fn lease_states_have_stable_wire_names() {
        assert_eq!(
            serde_json::to_string(&LeaseState::Starting).unwrap(),
            "\"starting\""
        );
        assert_eq!(
            serde_json::to_string(&LeaseState::Ready).unwrap(),
            "\"ready\""
        );
    }

    #[test]
    fn unknown_versions_corruption_and_unknown_fields_are_safe_errors() {
        assert!(matches!(
            decode(br#"{"format_version":2}"#),
            Err(Error::UnsupportedVersion(2))
        ));
        assert!(matches!(decode(b"not json"), Err(Error::InvalidJson(_))));
        assert!(matches!(decode(br#"{"format_version":1,"aliases":{},"leases":{},"selected_servers":{},"last_synchronized_at_unix_ms":null,"pending_operations":[],"surprise":true}"#), Err(Error::InvalidJson(_))));
    }
}
