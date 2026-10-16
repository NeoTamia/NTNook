//! Per-run mDNS announcements for HTTP access through Caddy.

use std::io;
use std::net::Ipv4Addr;
use std::time::{Duration, Instant};

use mdns_sd::{DaemonEvent, IfKind, Receiver, ServiceDaemon, ServiceInfo};
use uuid::Uuid;

pub(crate) fn hostname(localhost: &str) -> String {
    let name = localhost.strip_suffix(".localhost").unwrap_or(localhost);
    // Match mDNS's canonical .local suffix, including names such as api.local.
    format!("{}.local", name.trim_end_matches(".local"))
}

pub(crate) fn addresses() -> io::Result<Vec<Ipv4Addr>> {
    let mut addresses: Vec<_> = if_addrs::get_if_addrs()?
        .into_iter()
        .filter(|interface| interface.is_oper_up())
        .filter_map(|interface| match interface.addr {
            if_addrs::IfAddr::V4(address) if !address.ip.is_loopback() => Some(address.ip),
            _ => None,
        })
        .collect();
    addresses.sort_unstable();
    addresses.dedup();
    Ok(addresses)
}

pub(crate) struct Advertisement {
    daemon: ServiceDaemon,
    events: Receiver<DaemonEvent>,
    hostname: String,
    port: u16,
    owner: Uuid,
    last_owner_check: Instant,
}

impl Advertisement {
    #[cfg(all(test, unix))]
    pub(crate) fn daemon_for_test(&self) -> ServiceDaemon {
        self.daemon.clone()
    }

    pub(crate) fn start(localhost: &str, port: u16, owner: Uuid) -> mdns_sd::Result<Self> {
        Self::register(ServiceDaemon::new()?, localhost, port, owner)
    }

    pub(crate) fn is_displaced(&mut self, store: &crate::state::Store) -> bool {
        if self.last_owner_check.elapsed() < Duration::from_millis(250) {
            return false;
        }
        self.last_owner_check = Instant::now();
        // A transient read failure does not prove that ownership was revoked.
        store.load().is_ok_and(|registry| {
            !registry
                .leases
                .get(&self.owner)
                .is_some_and(|lease| lease.lan)
        })
    }

    fn register(
        daemon: ServiceDaemon,
        localhost: &str,
        port: u16,
        owner: Uuid,
    ) -> mdns_sd::Result<Self> {
        // Keep the guard alive during setup too, so failures shut the daemon down.
        let events = match daemon.monitor() {
            Ok(events) => events,
            Err(error) => {
                let _ = daemon.shutdown();
                return Err(error);
            }
        };
        let advertisement = Self {
            daemon,
            events,
            hostname: format!("{}.", hostname(localhost)),
            port,
            owner,
            last_owner_check: Instant::now(),
        };
        // The LAN Caddy route accepts IPv4 clients.
        advertisement.daemon.disable_interface(IfKind::IPv6)?;
        advertisement.daemon.disable_interface(IfKind::LoopbackV4)?;
        let service = ServiceInfo::new(
            "_http._tcp.local.",
            &format!("nook-{owner}"),
            &advertisement.hostname,
            "",
            port,
            &[("path", "/")][..],
        )?
        .enable_addr_auto();
        advertisement.daemon.register(service)?;
        Ok(advertisement)
    }

    pub(crate) fn warnings(&self) -> impl Iterator<Item = String> + '_ {
        self.events.try_iter().filter_map(|event| match event {
            DaemonEvent::Error(error) => Some(format!(
                "LAN name discovery failed: {error}"
            )),
            DaemonEvent::NameChange(change) if change.original == self.hostname => Some(format!(
                "LAN name {} conflicts on {}; mDNS renamed it to {} (service port {}). Restart with another --name so the Caddy route and mDNS name agree",
                self.hostname.trim_end_matches('.'),
                change.intf_name,
                change.new_name.trim_end_matches('.'),
                self.port,
            )),
            _ => None,
        })
    }
}

impl Drop for Advertisement {
    fn drop(&mut self) {
        // shutdown sends goodbye records before acknowledging completion.
        if let Ok(status) = self.daemon.shutdown() {
            let _ = status.recv_timeout(Duration::from_secs(1));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mdns_sd::{DaemonStatus, HostnameResolutionEvent, ServiceEvent};
    use std::net::UdpSocket;
    use std::time::Instant;

    #[test]
    fn lan_names_preserve_project_labels() {
        assert_eq!(hostname("api.localhost"), "api.local");
        assert_eq!(hostname("api.project.localhost"), "api.project.local");
        assert_eq!(hostname("api.localhost.localhost"), "api.localhost.local");
        assert_eq!(hostname("api.local.localhost"), "api.local");
    }

    #[test]
    fn mdns_resolves_the_hostname_and_withdraws_the_service_on_drop() {
        // Exercise real multicast on loopback at an ephemeral UDP port; no LAN
        // connection or system mDNS responder is required for this test.
        let socket = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let port = socket.local_addr().unwrap().port();
        drop(socket);
        let daemon = ServiceDaemon::new_with_port(port).unwrap();
        let status = daemon.clone();
        let owner = Uuid::new_v4();
        let local_name = format!("test-{owner}.localhost");
        let expected_host = format!("{}.", hostname(&local_name));
        let expected_service = format!("nook-{owner}._http._tcp.local.");
        let advertisement = Advertisement::register(daemon, &local_name, 12345, owner).unwrap();
        advertisement.daemon.disable_interface(IfKind::All).unwrap();
        advertisement
            .daemon
            .enable_interface(IfKind::LoopbackV4)
            .unwrap();

        let client = ServiceDaemon::new_with_port(port).unwrap();
        client.disable_interface(IfKind::All).unwrap();
        client.enable_interface(IfKind::LoopbackV4).unwrap();
        let services = client.browse("_http._tcp.local.").unwrap();
        let hosts = client.resolve_hostname(&expected_host, Some(5000)).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let event = hosts
                .recv_deadline(deadline)
                .expect("mDNS hostname did not resolve");
            if let HostnameResolutionEvent::AddressesFound(name, addresses) = event {
                assert_eq!(name, expected_host);
                assert!(
                    addresses
                        .iter()
                        .all(|address| address.to_ip_addr().is_ipv4())
                );
                assert!(
                    addresses
                        .iter()
                        .any(|address| address.to_ip_addr().is_loopback())
                );
                break;
            }
        }
        loop {
            let event = services
                .recv_deadline(deadline)
                .expect("HTTP service did not resolve");
            if let ServiceEvent::ServiceResolved(service) = event
                && service.fullname == expected_service
            {
                assert_eq!(service.host, expected_host);
                assert_eq!(service.port, 12345);
                break;
            }
        }

        drop(advertisement);
        assert_eq!(
            status
                .status()
                .unwrap()
                .recv_timeout(Duration::from_secs(1))
                .unwrap(),
            DaemonStatus::Shutdown
        );
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let event = services
                .recv_deadline(deadline)
                .expect("mDNS goodbye was not received");
            if let ServiceEvent::ServiceRemoved(_, name) = event
                && name == expected_service
            {
                break;
            }
        }
        client
            .shutdown()
            .unwrap()
            .recv_timeout(Duration::from_secs(1))
            .unwrap();
    }
}
