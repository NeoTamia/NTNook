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
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RouteError(pub(crate) String);

impl fmt::Display for RouteError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

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
            PendingOperationKind::InstallRoute {
                hostname, owner_id, ..
            } => current_route(registry, hostname, *owner_id)
                .map_or(Ok(()), |route| routes.ensure(&route)),
            PendingOperationKind::RemoveRoute {
                hostname,
                owner_id,
                tls,
            } => routes.remove_if_owned(hostname, *owner_id, *tls),
            PendingOperationKind::StartProcess { lease_id }
            | PendingOperationKind::FinalizeLease { lease_id } => {
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
            queue_install(registry, &route.hostname, route.owner_id, route.tls);
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
    }
}

fn route_for_alias(alias: &Alias) -> RouteSpec {
    RouteSpec {
        owner_id: alias.id,
        hostname: alias.hostname.clone(),
        target: alias.target.clone(),
        scheme: alias.scheme,
        tls: alias.tls,
    }
}

fn queue_install(registry: &mut Registry, hostname: &str, owner_id: Uuid, tls: bool) {
    registry.pending_operations.push(PendingOperation {
        id: Uuid::new_v4(),
        kind: PendingOperationKind::InstallRoute {
            hostname: hostname.to_owned(),
            owner_id,
            tls,
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
            } => ("install", hostname.clone(), *owner_id, *tls),
            PendingOperationKind::RemoveRoute {
                hostname,
                owner_id,
                tls,
            } => ("remove", hostname.clone(), *owner_id, *tls),
            PendingOperationKind::StartProcess { lease_id } => {
                ("start", String::new(), *lease_id, false)
            }
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

    use super::{RouteBackend, RouteError, RouteSpec, reconcile, reconcile_store};
    use crate::process::Liveness;
    use crate::state::{Alias, Lease, LeaseState, Registry, Scheme, Store, decode};
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
