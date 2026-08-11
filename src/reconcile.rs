//! Recovery of partial operations, dead leases, and missing managed routes.
#![allow(dead_code)]

use std::collections::BTreeSet;
use std::fmt;

use uuid::Uuid;

use crate::process::Liveness;
use crate::state::{Alias, Lease, PendingOperation, PendingOperationKind, Registry, Scheme, Store};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RouteSpec {
    pub(crate) owner_id: Uuid,
    pub(crate) hostname: String,
    pub(crate) target: String,
    pub(crate) scheme: Scheme,
    pub(crate) tls: bool,
    pub(crate) replace_existing: bool,
    pub(crate) preserve_host: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RouteError(pub(crate) String);

impl fmt::Display for RouteError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for RouteError {}

pub(crate) trait RouteBackend {
    fn ensure(&mut self, route: &RouteSpec) -> Result<(), RouteError>;
    fn remove_if_owned(
        &mut self,
        hostname: &str,
        owner_id: Uuid,
        tls: bool,
    ) -> Result<(), RouteError>;
}

#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Report {
    pub(crate) restored: usize,
    pub(crate) removed_dead_leases: usize,
    pub(crate) completed_operations: usize,
    pub(crate) warnings: Vec<String>,
}

#[derive(Debug, Clone)]
pub(crate) struct AliasRequest {
    pub(crate) hostname: String,
    pub(crate) target: String,
    pub(crate) scheme: Scheme,
    pub(crate) tls: bool,
    pub(crate) preserve_host: bool,
    pub(crate) force: bool,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum AliasError {
    State(String),
    Route(RouteError),
    Conflict(String),
}

impl fmt::Display for AliasError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::State(error) => formatter.write_str(error),
            Self::Route(error) => error.fmt(formatter),
            Self::Conflict(hostname) => write!(
                formatter,
                "hostname `{hostname}` is already managed by Nook; use --force to replace it"
            ),
        }
    }
}

impl std::error::Error for AliasError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Route(error) => Some(error),
            _ => None,
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct AliasOutcome {
    pub(crate) alias: Alias,
    pub(crate) warnings: Vec<String>,
}

pub(crate) fn set_alias(
    store: &Store,
    routes: &mut impl RouteBackend,
    request: AliasRequest,
) -> Result<AliasOutcome, AliasError> {
    let _operations = store
        .lock_operations()
        .map_err(|error| AliasError::State(error.to_string()))?;
    let conflicts = store
        .mutate(|registry| {
            let aliases = registry
                .aliases
                .values()
                .filter(|alias| alias.hostname == request.hostname)
                .map(|alias| (alias.id, alias.tls));
            let leases = registry
                .leases
                .values()
                .filter(|lease| lease.hostname == request.hostname)
                .map(|lease| (lease.id, lease.tls));
            Ok(aliases.chain(leases).collect::<Vec<_>>())
        })
        .map_err(|error| AliasError::State(error.to_string()))?;
    if !conflicts.is_empty() && !request.force {
        return Err(AliasError::Conflict(request.hostname));
    }

    let alias = Alias {
        id: Uuid::new_v4(),
        hostname: request.hostname,
        target: request.target,
        scheme: request.scheme,
        tls: request.tls,
        preserve_host: request.preserve_host,
    };
    let operation_id = Uuid::new_v4();
    store
        .mutate(|registry| {
            registry.pending_operations.push(PendingOperation {
                id: operation_id,
                kind: PendingOperationKind::InstallRoute {
                    hostname: alias.hostname.clone(),
                    target: alias.target.clone(),
                    scheme: alias.scheme,
                    owner_id: alias.id,
                    tls: alias.tls,
                },
            });
            Ok(())
        })
        .map_err(|error| AliasError::State(error.to_string()))?;
    routes
        .ensure(&RouteSpec {
            owner_id: alias.id,
            hostname: alias.hostname.clone(),
            target: alias.target.clone(),
            scheme: alias.scheme,
            tls: alias.tls,
            replace_existing: request.force,
            preserve_host: request.preserve_host,
        })
        .map_err(AliasError::Route)?;
    let cleanup: Vec<_> = conflicts
        .iter()
        .map(|(owner, tls)| {
            (
                *owner,
                *tls,
                routes.remove_if_owned(&alias.hostname, *owner, *tls),
            )
        })
        .collect();
    let mut warnings = Vec::new();
    if !conflicts.is_empty() {
        warnings.push(format!(
            "replaced existing Nook route for {}",
            alias.hostname
        ));
    }
    store
        .mutate(|registry| {
            let old_owners: BTreeSet<_> = conflicts.iter().map(|(owner, _)| *owner).collect();
            registry
                .aliases
                .retain(|_, old| !old_owners.contains(&old.id));
            registry
                .leases
                .retain(|owner, _| !old_owners.contains(owner));
            registry.pending_operations.retain(|operation| {
                operation.id == operation_id
                    || pending_owner(&operation.kind)
                        .is_none_or(|owner| !old_owners.contains(&owner))
            });
            registry
                .aliases
                .insert(alias.hostname.clone(), alias.clone());
            registry
                .pending_operations
                .retain(|operation| operation.id != operation_id);
            for (owner_id, tls, result) in &cleanup {
                if let Err(error) = result {
                    warnings.push(format!(
                        "cleanup of previous route is pending: {error}; run `nook prune` to retry"
                    ));
                    queue_remove(registry, &alias.hostname, *owner_id, *tls);
                }
            }
            Ok(())
        })
        .map_err(|error| AliasError::State(error.to_string()))?;
    Ok(AliasOutcome { alias, warnings })
}

pub(crate) fn remove_alias(
    store: &Store,
    routes: &mut impl RouteBackend,
    hostname: &str,
) -> Result<Vec<String>, AliasError> {
    let _operations = store
        .lock_operations()
        .map_err(|error| AliasError::State(error.to_string()))?;
    let removal = store
        .mutate(|registry| {
            let Some(alias) = registry.aliases.remove(hostname) else {
                return Ok(None);
            };
            let operation_id = Uuid::new_v4();
            registry.pending_operations.push(PendingOperation {
                id: operation_id,
                kind: PendingOperationKind::RemoveRoute {
                    hostname: alias.hostname.clone(),
                    owner_id: alias.id,
                    tls: alias.tls,
                },
            });
            Ok(Some((alias, operation_id)))
        })
        .map_err(|error| AliasError::State(error.to_string()))?;
    let Some((alias, operation_id)) = removal else {
        return Ok(Vec::new());
    };
    match routes.remove_if_owned(&alias.hostname, alias.id, alias.tls) {
        Ok(()) => {
            store
                .mutate(|registry| {
                    registry
                        .pending_operations
                        .retain(|operation| operation.id != operation_id);
                    Ok(())
                })
                .map_err(|error| AliasError::State(error.to_string()))?;
            Ok(Vec::new())
        }
        Err(error) => Ok(vec![format!(
            "alias cleanup is pending: {error}; run `nook prune` to retry"
        )]),
    }
}

pub(crate) fn list_aliases(store: &Store) -> Result<Vec<Alias>, AliasError> {
    store
        .load()
        .map(|registry| registry.aliases.into_values().collect())
        .map_err(|error| AliasError::State(error.to_string()))
}

fn pending_owner(kind: &PendingOperationKind) -> Option<Uuid> {
    match kind {
        PendingOperationKind::InstallRoute { owner_id, .. }
        | PendingOperationKind::RestoreRoute { owner_id, .. }
        | PendingOperationKind::RemoveRoute { owner_id, .. }
        | PendingOperationKind::StartProcess { owner_id, .. } => Some(*owner_id),
        PendingOperationKind::FinalizeLease { lease_id } => Some(*lease_id),
    }
}

pub(crate) fn reconcile(
    registry: &mut Registry,
    routes: &mut impl RouteBackend,
    mut liveness: impl FnMut(&Lease) -> Liveness,
) -> Report {
    let mut report = Report::default();
    retry_pending(registry, routes, &mut liveness, &mut report);

    let leases: Vec<_> = registry.leases.values().cloned().collect();
    for lease in leases {
        match liveness(&lease) {
            Liveness::Alive => {
                restore(route_for_lease(&lease), registry, routes, &mut report);
            }
            Liveness::Dead => {
                registry.leases.remove(&lease.id);
                match routes.remove_if_owned(&lease.hostname, lease.id, lease.tls) {
                    Ok(()) => report.removed_dead_leases += 1,
                    Err(error) => {
                        queue_remove(registry, &lease.hostname, lease.id, lease.tls);
                        report
                            .warnings
                            .push(format!("cleanup of {} is pending: {error}", lease.hostname));
                    }
                }
            }
            Liveness::Indeterminate => report.warnings.push(format!(
                "process identity for {} is temporarily indeterminate",
                lease.hostname
            )),
        }
    }

    let aliases: Vec<_> = registry.aliases.values().cloned().collect();
    for alias in aliases {
        restore(route_for_alias(&alias), registry, routes, &mut report);
    }
    deduplicate_pending(registry);
    report
}

pub(crate) fn reconcile_store(
    store: &Store,
    routes: &mut impl RouteBackend,
    mut liveness: impl FnMut(&Lease) -> Liveness,
) -> Result<Report, crate::state::Error> {
    store.mutate(|registry| Ok(reconcile(registry, routes, &mut liveness)))
}

fn retry_pending(
    registry: &mut Registry,
    routes: &mut impl RouteBackend,
    liveness: &mut impl FnMut(&Lease) -> Liveness,
    report: &mut Report,
) {
    let pending = std::mem::take(&mut registry.pending_operations);
    for operation in pending {
        let result = match &operation.kind {
            PendingOperationKind::RestoreRoute {
                hostname, owner_id, ..
            } => current_route(registry, hostname, *owner_id)
                .map_or(Ok(()), |route| routes.ensure(&route)),
            PendingOperationKind::InstallRoute {
                hostname,
                owner_id,
                tls,
                ..
            }
            | PendingOperationKind::StartProcess {
                hostname,
                owner_id,
                tls,
                ..
            } => routes.remove_if_owned(hostname, *owner_id, *tls),
            PendingOperationKind::RemoveRoute {
                hostname,
                owner_id,
                tls,
            } => routes.remove_if_owned(hostname, *owner_id, *tls),
            PendingOperationKind::FinalizeLease { lease_id } => {
                match registry.leases.get(lease_id) {
                    Some(lease) if liveness(lease) == Liveness::Indeterminate => {
                        registry.pending_operations.push(operation);
                        continue;
                    }
                    _ => Ok(()),
                }
            }
        };
        match result {
            Ok(()) => report.completed_operations += 1,
            Err(error) => {
                report.warnings.push(format!(
                    "recovery operation {} is pending: {error}",
                    operation.id
                ));
                registry.pending_operations.push(operation);
            }
        }
    }
}

fn restore(
    route: RouteSpec,
    registry: &mut Registry,
    routes: &mut impl RouteBackend,
    report: &mut Report,
) {
    match routes.ensure(&route) {
        Ok(()) => report.restored += 1,
        Err(error) => {
            queue_restore(registry, &route);
            report.warnings.push(format!(
                "restoration of {} is pending: {error}",
                route.hostname
            ));
        }
    }
}

fn current_route(registry: &Registry, hostname: &str, owner_id: Uuid) -> Option<RouteSpec> {
    registry
        .leases
        .get(&owner_id)
        .filter(|lease| lease.hostname == hostname)
        .map(route_for_lease)
        .or_else(|| {
            registry
                .aliases
                .values()
                .find(|alias| alias.id == owner_id && alias.hostname == hostname)
                .map(route_for_alias)
        })
}

fn route_for_lease(lease: &Lease) -> RouteSpec {
    RouteSpec {
        owner_id: lease.id,
        hostname: lease.hostname.clone(),
        target: lease.target.clone(),
        scheme: lease.scheme,
        tls: lease.tls,
        replace_existing: false,
        preserve_host: false,
    }
}

fn route_for_alias(alias: &Alias) -> RouteSpec {
    RouteSpec {
        owner_id: alias.id,
        hostname: alias.hostname.clone(),
        target: alias.target.clone(),
        scheme: alias.scheme,
        tls: alias.tls,
        replace_existing: false,
        preserve_host: alias.preserve_host,
    }
}

fn queue_restore(registry: &mut Registry, route: &RouteSpec) {
    registry.pending_operations.push(PendingOperation {
        id: Uuid::new_v4(),
        kind: PendingOperationKind::RestoreRoute {
            hostname: route.hostname.clone(),
            target: route.target.clone(),
            scheme: route.scheme,
            owner_id: route.owner_id,
            tls: route.tls,
        },
    });
}

fn queue_remove(registry: &mut Registry, hostname: &str, owner_id: Uuid, tls: bool) {
    registry.pending_operations.push(PendingOperation {
        id: Uuid::new_v4(),
        kind: PendingOperationKind::RemoveRoute {
            hostname: hostname.to_owned(),
            owner_id,
            tls,
        },
    });
}

fn deduplicate_pending(registry: &mut Registry) {
    let mut seen = BTreeSet::new();
    registry.pending_operations.retain(|operation| {
        let key = match &operation.kind {
            PendingOperationKind::InstallRoute {
                hostname,
                owner_id,
                tls,
                ..
            } => ("install", hostname.clone(), *owner_id, *tls),
            PendingOperationKind::RestoreRoute {
                hostname,
                owner_id,
                tls,
                ..
            } => ("restore", hostname.clone(), *owner_id, *tls),
            PendingOperationKind::RemoveRoute {
                hostname,
                owner_id,
                tls,
            } => ("remove", hostname.clone(), *owner_id, *tls),
            PendingOperationKind::StartProcess {
                hostname,
                owner_id,
                tls,
                ..
            } => ("start", hostname.clone(), *owner_id, *tls),
            PendingOperationKind::FinalizeLease { lease_id } => {
                ("finalize", String::new(), *lease_id, false)
            }
        };
        seen.insert(key)
    });
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::{
        AliasError, AliasRequest, RouteBackend, RouteError, RouteSpec, list_aliases, reconcile,
        reconcile_store, remove_alias, set_alias,
    };
    use crate::process::Liveness;
    use crate::state::{
        Alias, Lease, LeaseState, PendingOperation, PendingOperationKind, Registry, Scheme, Store,
        decode,
    };
    use uuid::Uuid;

    #[derive(Default)]
    struct Routes {
        owners: BTreeMap<String, Uuid>,
        unavailable: bool,
    }

    impl RouteBackend for Routes {
        fn ensure(&mut self, route: &RouteSpec) -> Result<(), RouteError> {
            if self.unavailable {
                return Err(RouteError("Caddy unavailable".into()));
            }
            self.owners.insert(route.hostname.clone(), route.owner_id);
            Ok(())
        }

        fn remove_if_owned(
            &mut self,
            hostname: &str,
            owner_id: Uuid,
            _tls: bool,
        ) -> Result<(), RouteError> {
            if self.unavailable {
                return Err(RouteError("Caddy unavailable".into()));
            }
            if self.owners.get(hostname) == Some(&owner_id) {
                self.owners.remove(hostname);
            }
            Ok(())
        }
    }

    #[test]
    fn restores_aliases_and_live_runs_idempotently() {
        let mut registry = Registry::empty();
        let alias = alias("alias.localhost");
        let lease = lease("run.localhost");
        registry.aliases.insert(alias.hostname.clone(), alias);
        registry.leases.insert(lease.id, lease);
        let mut routes = Routes::default();
        reconcile(&mut registry, &mut routes, |_| Liveness::Alive);
        reconcile(&mut registry, &mut routes, |_| Liveness::Alive);
        assert_eq!(routes.owners.len(), 2);
        assert!(registry.pending_operations.is_empty());
    }

    #[test]
    fn recovers_journals_left_at_every_external_mutation_boundary() {
        let owner = Uuid::new_v4();
        let route_fields = || {
            (
                "boundary.localhost".to_owned(),
                "http://127.0.0.1:3000".to_owned(),
            )
        };

        for kind in [
            {
                let (hostname, target) = route_fields();
                PendingOperationKind::InstallRoute {
                    hostname,
                    target,
                    scheme: Scheme::Http,
                    owner_id: owner,
                    tls: true,
                }
            },
            {
                let (hostname, target) = route_fields();
                PendingOperationKind::StartProcess {
                    hostname,
                    target,
                    scheme: Scheme::Http,
                    owner_id: owner,
                    tls: true,
                }
            },
            PendingOperationKind::RemoveRoute {
                hostname: "boundary.localhost".into(),
                owner_id: owner,
                tls: true,
            },
        ] {
            let mut registry = Registry::empty();
            registry.pending_operations.push(PendingOperation {
                id: Uuid::new_v4(),
                kind,
            });
            let mut routes = Routes {
                owners: BTreeMap::from([("boundary.localhost".into(), owner)]),
                unavailable: false,
            };
            reconcile(&mut registry, &mut routes, |_| Liveness::Dead);
            assert!(registry.pending_operations.is_empty());
            assert!(routes.owners.is_empty());
        }

        let alias = alias("restore.localhost");
        let mut registry = Registry::empty();
        registry
            .aliases
            .insert(alias.hostname.clone(), alias.clone());
        registry.pending_operations.push(PendingOperation {
            id: Uuid::new_v4(),
            kind: PendingOperationKind::RestoreRoute {
                hostname: alias.hostname.clone(),
                target: alias.target.clone(),
                scheme: alias.scheme,
                owner_id: alias.id,
                tls: alias.tls,
            },
        });
        let mut routes = Routes::default();
        reconcile(&mut registry, &mut routes, |_| Liveness::Dead);
        assert!(registry.pending_operations.is_empty());
        assert_eq!(routes.owners[&alias.hostname], alias.id);

        let lease = lease("finalize.localhost");
        let mut registry = Registry::empty();
        registry.leases.insert(lease.id, lease.clone());
        registry.pending_operations.push(PendingOperation {
            id: Uuid::new_v4(),
            kind: PendingOperationKind::FinalizeLease { lease_id: lease.id },
        });
        let mut routes = Routes {
            owners: BTreeMap::from([(lease.hostname.clone(), lease.id)]),
            unavailable: false,
        };
        let report = reconcile(&mut registry, &mut routes, |_| Liveness::Dead);
        assert_eq!(report.completed_operations, 1);
        assert_eq!(report.removed_dead_leases, 1);
        assert!(registry.pending_operations.is_empty());
        assert!(registry.leases.is_empty());
        assert!(routes.owners.is_empty());
    }

    #[test]
    fn dead_lease_cleanup_never_removes_a_new_owner() {
        let mut registry = Registry::empty();
        let lease = lease("api.localhost");
        let old_owner = lease.id;
        registry.leases.insert(old_owner, lease);
        let new_owner = Uuid::new_v4();
        let mut routes = Routes {
            owners: BTreeMap::from([("api.localhost".into(), new_owner)]),
            unavailable: false,
        };
        reconcile(&mut registry, &mut routes, |_| Liveness::Dead);
        assert_eq!(routes.owners.get("api.localhost"), Some(&new_owner));
        assert!(!registry.leases.contains_key(&old_owner));
    }

    #[test]
    fn unavailable_cleanup_is_persisted_and_converges_on_retry() {
        let mut registry = Registry::empty();
        let lease = lease("api.localhost");
        let owner = lease.id;
        registry.leases.insert(owner, lease);
        let mut routes = Routes {
            owners: BTreeMap::from([("api.localhost".into(), owner)]),
            unavailable: true,
        };
        let report = reconcile(&mut registry, &mut routes, |_| Liveness::Dead);
        assert_eq!(report.warnings.len(), 1);
        assert_eq!(registry.pending_operations.len(), 1);

        routes.unavailable = false;
        reconcile(&mut registry, &mut routes, |_| Liveness::Dead);
        assert!(registry.pending_operations.is_empty());
        assert!(!routes.owners.contains_key("api.localhost"));
    }

    #[test]
    fn indeterminate_process_is_preserved_without_route_mutation() {
        let mut registry = Registry::empty();
        let lease = lease("api.localhost");
        let owner = lease.id;
        registry.leases.insert(owner, lease);
        let mut routes = Routes::default();
        reconcile(&mut registry, &mut routes, |_| Liveness::Indeterminate);
        assert!(registry.leases.contains_key(&owner));
        assert!(routes.owners.is_empty());
    }

    #[test]
    fn backend_failure_is_atomically_persisted_for_the_next_process() {
        let directory = std::env::temp_dir().join(format!("nook-reconcile-{}", Uuid::new_v4()));
        let path = directory.join("state.json");
        let store = Store::new(path.clone());
        let lease = lease("api.localhost");
        let owner = lease.id;
        store
            .mutate(|registry| {
                registry.leases.insert(owner, lease);
                Ok(())
            })
            .unwrap();
        let mut routes = Routes {
            owners: BTreeMap::from([("api.localhost".into(), owner)]),
            unavailable: true,
        };
        reconcile_store(&store, &mut routes, |_| Liveness::Dead).unwrap();
        let persisted = decode(&std::fs::read(&path).unwrap()).unwrap();
        assert!(!persisted.leases.contains_key(&owner));
        assert_eq!(persisted.pending_operations.len(), 1);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn alias_is_persisted_listed_and_requires_force_to_replace() {
        let (store, path) = temporary_store();
        let mut routes = Routes::default();
        let created = set_alias(&store, &mut routes, alias_request(false)).unwrap();
        let aliases = list_aliases(&store).unwrap();
        assert_eq!(aliases.len(), 1);
        assert_eq!(aliases[0], created.alias);
        assert!(matches!(
            set_alias(&store, &mut routes, alias_request(false)),
            Err(AliasError::Conflict(hostname)) if hostname == "alias.localhost"
        ));
        let replacement = set_alias(&store, &mut routes, alias_request(true)).unwrap();
        assert_eq!(replacement.warnings.len(), 1);
        assert_eq!(routes.owners["alias.localhost"], replacement.alias.id);
        assert_eq!(list_aliases(&store).unwrap(), [replacement.alias]);
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn alias_removal_is_idempotent_and_cannot_remove_a_foreign_owner() {
        let (store, path) = temporary_store();
        let mut routes = Routes::default();
        let created = set_alias(&store, &mut routes, alias_request(false)).unwrap();
        let foreign_owner = Uuid::new_v4();
        routes
            .owners
            .insert(created.alias.hostname.clone(), foreign_owner);
        assert!(
            remove_alias(&store, &mut routes, "alias.localhost")
                .unwrap()
                .is_empty()
        );
        assert!(
            remove_alias(&store, &mut routes, "alias.localhost")
                .unwrap()
                .is_empty()
        );
        assert_eq!(routes.owners["alias.localhost"], foreign_owner);
        assert!(list_aliases(&store).unwrap().is_empty());
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn failed_alias_cleanup_is_journaled_without_restoring_the_alias() {
        let (store, path) = temporary_store();
        let mut routes = Routes::default();
        set_alias(&store, &mut routes, alias_request(false)).unwrap();
        routes.unavailable = true;
        assert_eq!(
            remove_alias(&store, &mut routes, "alias.localhost")
                .unwrap()
                .len(),
            1
        );
        let registry = decode(&std::fs::read(&path).unwrap()).unwrap();
        assert!(registry.aliases.is_empty());
        assert_eq!(registry.pending_operations.len(), 1);
        routes.unavailable = false;
        reconcile_store(&store, &mut routes, |_| Liveness::Dead).unwrap();
        assert!(
            decode(&std::fs::read(&path).unwrap())
                .unwrap()
                .pending_operations
                .is_empty()
        );
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    fn alias_request(force: bool) -> AliasRequest {
        AliasRequest {
            hostname: "alias.localhost".into(),
            target: "http://127.0.0.1:9".into(),
            scheme: Scheme::Http,
            tls: true,
            preserve_host: false,
            force,
        }
    }

    fn temporary_store() -> (Store, std::path::PathBuf) {
        let directory = std::env::temp_dir().join(format!("nook-alias-{}", Uuid::new_v4()));
        let path = directory.join("state.json");
        (Store::new(path.clone()), path)
    }

    fn alias(hostname: &str) -> Alias {
        Alias {
            id: Uuid::new_v4(),
            hostname: hostname.into(),
            target: "http://127.0.0.1:3000".into(),
            scheme: Scheme::Http,
            tls: true,
            preserve_host: false,
        }
    }

    fn lease(hostname: &str) -> Lease {
        Lease {
            id: Uuid::new_v4(),
            hostname: hostname.into(),
            target: "http://127.0.0.1:3001".into(),
            scheme: Scheme::Http,
            tls: true,
            pid: 1,
            pgid: 1,
            process_start_time_ticks: 1,
            state: LeaseState::Ready,
        }
    }
}
