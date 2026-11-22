//! Bounded DNS-SD discovery. Advertisements are untrusted connection candidates.

use mdns_sd::{DaemonEvent, ResolvedService, ServiceDaemon, ServiceEvent};
use serde::Serialize;
use std::{
    collections::{BTreeMap, HashSet},
    net::IpAddr,
    time::{Duration, Instant},
};

pub const SERVICE_TYPE: &str = "_appdepot._tcp.local.";
const MAX_INSTANCES: usize = 64;

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ServerCandidate {
    pub name: String,
    pub origin: String,
}

fn candidate(
    name: &str,
    version: Option<&str>,
    scheme: Option<&str>,
    host: &str,
    port: u16,
    addresses: impl IntoIterator<Item = IpAddr>,
) -> Option<ServerCandidate> {
    if version != Some("1") || port == 0 || name.is_empty() || name.chars().any(char::is_control) {
        return None;
    }
    let mut addresses = addresses
        .into_iter()
        .filter(|addr| {
            !addr.is_unspecified()
                && !addr.is_multicast()
                && !addr.is_loopback()
                && match addr {
                    IpAddr::V4(addr) => !addr.is_broadcast() && !addr.is_link_local(),
                    IpAddr::V6(addr) => !addr.is_unicast_link_local(),
                }
        })
        .collect::<Vec<_>>();
    addresses.sort_unstable();
    let address = addresses.first()?;
    let origin = match scheme {
        Some("http") => format!("http://{}/", std::net::SocketAddr::new(*address, port)),
        Some("https") => {
            let host = host.strip_suffix('.').unwrap_or(host);
            if host.len() > 253
                || !host.split('.').all(|label| {
                    !label.is_empty()
                        && label.len() <= 63
                        && !label.starts_with('-')
                        && !label.ends_with('-')
                        && label
                            .bytes()
                            .all(|b| b.is_ascii_alphanumeric() || b == b'-')
                })
            {
                return None;
            }
            // Retain the advertised hostname so HTTPS uses normal DNS and TLS validation.
            format!("https://{}:{port}/", host.to_ascii_lowercase())
        }
        _ => return None,
    };
    Some(ServerCandidate {
        name: name.into(),
        origin,
    })
}

fn update(instances: &mut BTreeMap<String, ServerCandidate>, event: ServiceEvent) {
    match event {
        ServiceEvent::ServiceResolved(service) => {
            let full = service.get_fullname();
            let key = full.to_ascii_lowercase();
            if !key.ends_with(SERVICE_TYPE) {
                return;
            }
            if let Some(candidate) = resolved(&service) {
                if instances.len() < MAX_INSTANCES || instances.contains_key(&key) {
                    instances.insert(key, candidate);
                }
            } else {
                instances.remove(&key);
            }
        }
        ServiceEvent::ServiceRemoved(_, full) => {
            instances.remove(&full.to_ascii_lowercase());
        }
        _ => {}
    }
}

fn resolved(service: &ResolvedService) -> Option<ServerCandidate> {
    candidate(
        &mdns_sd::instance_name_unescaped(service.get_fullname()),
        service.get_property_val_str("v"),
        service.get_property_val_str("scheme"),
        service.get_hostname(),
        service.get_port(),
        service
            .get_addresses()
            .iter()
            .map(mdns_sd::ScopedIp::to_ip_addr),
    )
}

/// Return a fresh snapshot of DNS-SD candidates. Does not contact their HTTP APIs.
pub fn discover(duration: Duration) -> Result<Vec<ServerCandidate>, mdns_sd::Error> {
    let daemon = ServiceDaemon::new()?;
    let search = (|| {
        let monitor = daemon.monitor()?;
        let receiver = daemon.browse(SERVICE_TYPE)?;
        let deadline = Instant::now() + duration.min(Duration::from_secs(10));
        let mut instances = BTreeMap::new();
        while let Some(left) = deadline.checked_duration_since(Instant::now()) {
            match receiver.recv_timeout(left) {
                Ok(event) => update(&mut instances, event),
                Err(_) => break,
            }
        }
        if instances.is_empty()
            && let Some(DaemonEvent::Error(error)) = monitor
                .try_iter()
                .find(|event| matches!(event, DaemonEvent::Error(_)))
        {
            return Err(error);
        }
        let mut seen = HashSet::new();
        Ok(instances
            .into_values()
            .filter(|item| seen.insert(item.origin.clone()))
            .collect())
    })();
    let _ = daemon.shutdown();
    search
}

#[cfg(test)]
mod tests {
    use super::*;
    use mdns_sd::ServiceInfo;

    fn event(name: &str, address: &str, txt: &[(&str, &str)]) -> ServiceEvent {
        let service =
            ServiceInfo::new(SERVICE_TYPE, name, "depot.local.", address, 8787, txt).unwrap();
        ServiceEvent::ServiceResolved(Box::new(service.as_resolved_service()))
    }

    #[test]
    fn snapshot_handles_multiple_duplicates_goodbyes_and_bad_metadata() {
        let mut rows = BTreeMap::new();
        let txt = &[("v", "1"), ("scheme", "http")];
        for event in [
            event("One", "192.168.1.2", txt),
            event("One", "192.168.1.2", txt),
            event("Two", "fd00::2", txt),
            event("Skipped", "192.168.1.3", &[("v", "2"), ("scheme", "http")]),
            event("Bad", "192.168.1.4", &[("v", "1"), ("scheme", "file")]),
            event("Link-local", "fe80::1", txt),
        ] {
            update(&mut rows, event);
        }
        assert_eq!(rows.len(), 2);
        assert_eq!(
            rows.values().next().unwrap().origin,
            "http://192.168.1.2:8787/"
        );
        assert_eq!(
            rows.values().last().unwrap().origin,
            "http://[fd00::2]:8787/"
        );
        for key in rows.keys().cloned().collect::<Vec<_>>() {
            update(
                &mut rows,
                ServiceEvent::ServiceRemoved(SERVICE_TYPE.into(), key),
            );
        }
        assert!(rows.is_empty());
    }

    #[test]
    #[ignore = "requires isolated interfaces with multicast enabled"]
    fn isolated_multicast_browse() {
        let advertiser = ServiceDaemon::new().unwrap();
        for (name, port) in [("Fixture One", 18787), ("Fixture Two", 18788)] {
            let info = ServiceInfo::new(
                SERVICE_TYPE,
                name,
                "fixture-depot.local.",
                "",
                port,
                &[("v", "1"), ("scheme", "http")][..],
            )
            .unwrap()
            .enable_addr_auto();
            advertiser.register(info).unwrap();
        }
        let found = discover(Duration::from_secs(4)).unwrap();
        assert_eq!(found.len(), 2, "{found:?}");
        for name in ["Fixture One", "Fixture Two"] {
            assert!(found.iter().any(|item| item.name == name), "{found:?}");
        }
        advertiser.shutdown().unwrap();
    }

    #[test]
    fn deterministic_address_order_and_tls_hostname() {
        let addresses = ["fd00::5".parse().unwrap(), "192.168.1.4".parse().unwrap()];
        assert_eq!(
            candidate(
                "Name",
                Some("1"),
                Some("http"),
                "Test.local.",
                80,
                addresses
            )
            .unwrap()
            .origin,
            "http://192.168.1.4:80/"
        );
        assert_eq!(
            candidate(
                "Name",
                Some("1"),
                Some("https"),
                "Test.local.",
                443,
                addresses
            )
            .unwrap()
            .origin,
            "https://test.local:443/"
        );
        assert!(
            candidate(
                "Name",
                Some("1"),
                Some("https"),
                "user@evil.local.",
                443,
                addresses
            )
            .is_none()
        );
    }
}
