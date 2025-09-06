use common::lease::{Event, EventType, Lease};
use log::error;
use serde::{Deserialize, Serialize};

use crate::network::manager::Cursor;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct LeaseWatchResult {
    pub events: Vec<Event>,
    pub snapshot: Vec<Lease>, // only used in etcd
    pub cursor: Cursor,       // Only used in etcd
}

#[derive(Debug)]
pub struct LeaseWatcher {
    pub own_lease: Lease,   // Lease with subnet of the local node
    pub leases: Vec<Lease>, // Leases from other nodes
}

fn same_subnet(ipv4_enabled: bool, ipv6_enabled: bool, a: &Lease, b: &Lease) -> bool {
    match (ipv4_enabled, ipv6_enabled) {
        (true, false) => a.subnet == b.subnet,
        (false, true) => a.ipv6_subnet == b.ipv6_subnet,
        (true, true) => a.subnet == b.subnet && a.ipv6_subnet == b.ipv6_subnet,
        (false, false) => a.subnet == b.subnet, // etcd case fallback
    }
}

impl LeaseWatcher {
    pub fn reset(&mut self, new_leases: Vec<Lease>) -> Vec<Event> {
        let mut batch = Vec::new();

        for new_lease in &new_leases {
            if same_subnet(
                new_lease.enable_ipv4,
                new_lease.enable_ipv6,
                &self.own_lease,
                new_lease,
            ) {
                continue;
            }

            let mut found = false;
            self.leases.retain(|old_lease| {
                if same_subnet(
                    old_lease.enable_ipv4,
                    old_lease.enable_ipv6,
                    old_lease,
                    new_lease,
                ) {
                    found = true;
                    false // remove this old lease
                } else {
                    true // keep it
                }
            });

            if !found {
                batch.push(Event {
                    event_type: EventType::Added,
                    lease: Some(new_lease.clone()),
                });
            }
        }

        for removed in &self.leases {
            batch.push(Event {
                event_type: EventType::Removed,
                lease: Some(removed.clone()),
            });
        }

        // Update internal state
        self.leases = new_leases;

        batch
    }

    pub fn update(&mut self, events: Vec<Event>) -> Vec<Event> {
        let mut batch = Vec::new();

        for event in events {
            let lease = match &event.lease {
                Some(lease) => lease,
                None => continue,
            };

            if same_subnet(lease.enable_ipv4, lease.enable_ipv6, &self.own_lease, lease) {
                continue;
            }

            match event.event_type {
                EventType::Added => batch.push(self.add(lease)),
                EventType::Removed => batch.push(self.remove(lease)),
            }
        }

        batch
    }

    fn add(&mut self, lease: &Lease) -> Event {
        for existing in &mut self.leases {
            if same_subnet(existing.enable_ipv4, existing.enable_ipv6, existing, lease) {
                *existing = lease.clone();
                return Event {
                    event_type: EventType::Added,
                    lease: Some(existing.clone()),
                };
            }
        }

        self.leases.push(lease.clone());
        Event {
            event_type: EventType::Added,
            lease: Some(lease.clone()),
        }
    }

    fn remove(&mut self, lease: &Lease) -> Event {
        for i in 0..self.leases.len() {
            if same_subnet(
                self.leases[i].enable_ipv4,
                self.leases[i].enable_ipv6,
                &self.leases[i],
                lease,
            ) {
                let removed = self.leases.remove(i);
                return Event {
                    event_type: EventType::Removed,
                    lease: Some(removed),
                };
            }
        }

        // Not found, but still return an event
        error!(
            "Removed subnet ({}) and ipv6 subnet ({:?}) were not found",
            lease.subnet, lease.ipv6_subnet
        );
        Event {
            event_type: EventType::Removed,
            lease: Some(lease.clone()),
        }
    }
}
