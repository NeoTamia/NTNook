//! Versioned persistent aliases, run leases, and recovery operations.
#![allow(dead_code)]

use std::collections::BTreeMap;
use std::env;
use std::ffi::OsString;
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
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
    Io { path: PathBuf, source: io::Error },
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
            Self::Io { path, source } => {
                write!(formatter, "state I/O error at {}: {source}", path.display())
            }
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::InvalidJson(error) => Some(error),
            Self::Io { source, .. } => Some(source),
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Alias {
    #[serde(default = "Uuid::new_v4")]
    pub(crate) id: Uuid,
    pub(crate) hostname: String,
    pub(crate) target: String,
    pub(crate) scheme: Scheme,
    #[serde(default = "default_true")]
    pub(crate) tls: bool,
    pub(crate) preserve_host: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Lease {
    pub(crate) id: Uuid,
    pub(crate) hostname: String,
    pub(crate) target: String,
    pub(crate) scheme: Scheme,
    #[serde(default = "default_true")]
    pub(crate) tls: bool,
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PendingOperation {
    pub(crate) id: Uuid,
    pub(crate) kind: PendingOperationKind,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum PendingOperationKind {
    InstallRoute {
        hostname: String,
        target: String,
        scheme: Scheme,
        owner_id: Uuid,
        #[serde(default = "default_true")]
        tls: bool,
    },
    RestoreRoute {
        hostname: String,
        target: String,
        scheme: Scheme,
        owner_id: Uuid,
        #[serde(default = "default_true")]
        tls: bool,
    },
    RemoveRoute {
        hostname: String,
        owner_id: Uuid,
        #[serde(default = "default_true")]
        tls: bool,
    },
    StartProcess {
        hostname: String,
        target: String,
        scheme: Scheme,
        owner_id: Uuid,
        #[serde(default = "default_true")]
        tls: bool,
    },
    FinalizeLease {
        lease_id: Uuid,
    },
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

fn default_true() -> bool {
    true
}

pub(crate) fn state_path() -> Result<PathBuf, Error> {
    state_path_with(|key| env::var_os(key))
}

#[derive(Debug, Clone)]
pub(crate) struct Store {
    path: PathBuf,
}

impl Store {
    pub(crate) fn new(path: PathBuf) -> Self {
        Self { path }
    }

    pub(crate) fn lock_operations(&self) -> Result<OperationGuard, Error> {
        let parent = self.path.parent().ok_or_else(|| {
            io_error(
                &self.path,
                io::Error::new(io::ErrorKind::InvalidInput, "state path has no parent"),
            )
        })?;
        fs::create_dir_all(parent).map_err(|source| io_error(parent, source))?;
        let path = self.path.with_extension("operations.lock");
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)
            .map_err(|source| io_error(&path, source))?;
        file.lock().map_err(|source| io_error(&path, source))?;
        Ok(OperationGuard { _file: file })
    }

    pub(crate) fn load(&self) -> Result<Registry, Error> {
        let parent = self.path.parent().ok_or_else(|| {
            io_error(
                &self.path,
                io::Error::new(io::ErrorKind::InvalidInput, "state path has no parent"),
            )
        })?;
        fs::create_dir_all(parent).map_err(|source| io_error(parent, source))?;
        let lock_path = self.path.with_extension("lock");
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&lock_path)
            .map_err(|source| io_error(&lock_path, source))?;
        lock.lock().map_err(|source| io_error(&lock_path, source))?;
        match fs::read(&self.path) {
            Ok(contents) => decode(&contents),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(Registry::empty()),
            Err(source) => Err(io_error(&self.path, source)),
        }
    }

    pub(crate) fn mutate<T>(
        &self,
        operation: impl FnOnce(&mut Registry) -> Result<T, Error>,
    ) -> Result<T, Error> {
        let parent = self.path.parent().ok_or_else(|| {
            io_error(
                &self.path,
                io::Error::new(io::ErrorKind::InvalidInput, "state path has no parent"),
            )
        })?;
        fs::create_dir_all(parent).map_err(|source| io_error(parent, source))?;
        let lock_path = self.path.with_extension("lock");
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&lock_path)
            .map_err(|source| io_error(&lock_path, source))?;
        lock.lock().map_err(|source| io_error(&lock_path, source))?;

        let temporary_path = self.path.with_extension("json.tmp");
        match fs::remove_file(&temporary_path) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(source) => return Err(io_error(&temporary_path, source)),
        }
        let mut registry = match fs::read(&self.path) {
            Ok(contents) => decode(&contents)?,
            Err(error) if error.kind() == io::ErrorKind::NotFound => Registry::empty(),
            Err(source) => return Err(io_error(&self.path, source)),
        };
        let result = operation(&mut registry)?;
        let bytes = serde_json::to_vec_pretty(&registry).map_err(Error::InvalidJson)?;
        let mut temporary = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary_path)
            .map_err(|source| io_error(&temporary_path, source))?;
        temporary
            .write_all(&bytes)
            .and_then(|()| temporary.flush())
            .and_then(|()| temporary.sync_all())
            .map_err(|source| io_error(&temporary_path, source))?;
        fs::rename(&temporary_path, &self.path).map_err(|source| io_error(&self.path, source))?;
        File::open(parent)
            .and_then(|directory| directory.sync_all())
            .map_err(|source| io_error(parent, source))?;
        Ok(result)
    }
}

pub(crate) struct OperationGuard {
    _file: File,
}

fn io_error(path: impl Into<PathBuf>, source: io::Error) -> Error {
    Error::Io {
        path: path.into(),
        source,
    }
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
        Alias, Error, LeaseState, PendingOperation, PendingOperationKind, Registry, Scheme, Store,
        decode, state_path_with,
    };
    use std::ffi::OsString;
    use std::fs;
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
                tls: true,
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

    #[test]
    fn legacy_v1_alias_without_owner_id_is_migrated_on_decode() {
        let registry = decode(br#"{"format_version":1,"aliases":{"api.localhost":{"hostname":"api.localhost","target":"http://127.0.0.1:3000","scheme":"http","tls":true,"preserve_host":false}},"leases":{},"selected_servers":{},"last_synchronized_at_unix_ms":null,"pending_operations":[]}"#).unwrap();
        assert_ne!(registry.aliases["api.localhost"].id, Uuid::nil());
    }

    #[test]
    fn atomic_mutations_recover_stale_temporary_file() {
        let directory = temporary_directory("recovery");
        let path = directory.join("state.json");
        fs::write(path.with_extension("json.tmp"), b"partial").unwrap();
        let store = Store::new(path.clone());
        store
            .mutate(|registry| {
                registry.last_synchronized_at_unix_ms = Some(42);
                Ok(())
            })
            .unwrap();
        assert!(!path.with_extension("json.tmp").exists());
        assert_eq!(
            decode(&fs::read(&path).unwrap())
                .unwrap()
                .last_synchronized_at_unix_ms,
            Some(42)
        );
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn concurrent_mutations_do_not_lose_updates() {
        let directory = temporary_directory("concurrent");
        let path = directory.join("state.json");
        let handles: Vec<_> = (0..8)
            .map(|index| {
                let store = Store::new(path.clone());
                std::thread::spawn(move || {
                    store.mutate(|registry| {
                        registry.aliases.insert(
                            format!("app-{index}"),
                            Alias {
                                id: Uuid::new_v4(),
                                hostname: format!("app-{index}.localhost"),
                                target: format!("http://127.0.0.1:{}", 3000 + index),
                                scheme: Scheme::Http,
                                tls: true,
                                preserve_host: false,
                            },
                        );
                        Ok(())
                    })
                })
            })
            .collect();
        for handle in handles {
            handle.join().unwrap().unwrap();
        }
        assert_eq!(decode(&fs::read(&path).unwrap()).unwrap().aliases.len(), 8);
        fs::remove_dir_all(directory).unwrap();
    }

    fn temporary_directory(label: &str) -> std::path::PathBuf {
        let path = std::env::temp_dir().join(format!(
            "nook-state-{label}-{}-{}",
            std::process::id(),
            Uuid::new_v4()
        ));
        fs::create_dir(&path).unwrap();
        path
    }
}
