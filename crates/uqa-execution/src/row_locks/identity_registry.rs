//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Stable table identities and explicitly retained transient key identities.

use super::{Arc, HashMap, LockRelationIdentity, Mutex, Weak};

struct Record {
    identity: LockRelationIdentity,
    permanent: bool,
    retained: Weak<Retained>,
}

struct State {
    next: u64,
    by_identity: HashMap<LockRelationIdentity, u64>,
    by_id: HashMap<u64, Record>,
}

pub(super) struct IdentityRegistry(Arc<Mutex<State>>);

impl Default for IdentityRegistry {
    fn default() -> Self {
        Self(Arc::new(Mutex::new(State {
            next: 1,
            by_identity: HashMap::new(),
            by_id: HashMap::new(),
        })))
    }
}

struct Retained {
    table: u64,
    registry: Weak<Mutex<State>>,
}

/// Keeps a complete key identity available while requests are built. Successful acquisitions retain their own handle through native release. Numeric keys obtained from this handle must not outlive their owner unless promoted through the permanent-key API.
#[derive(Clone)]
pub struct KeyReservationIdentity(Arc<Retained>);

impl std::fmt::Debug for KeyReservationIdentity {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("KeyReservationIdentity")
            .field("table", &self.table_key())
            .finish_non_exhaustive()
    }
}

impl KeyReservationIdentity {
    #[must_use]
    pub fn table_key(&self) -> u64 {
        self.0.table
    }
}

impl Drop for Retained {
    fn drop(&mut self) {
        let Some(registry) = self.registry.upgrade() else {
            return;
        };
        let mut state = registry.lock();
        if state
            .by_id
            .get(&self.table)
            .is_some_and(|record| !record.permanent)
        {
            let record = state.by_id.remove(&self.table).expect("retained identity");
            if state.by_identity.get(&record.identity) == Some(&self.table) {
                state.by_identity.remove(&record.identity);
            }
            // Reclaim high-water allocations geometrically, not once per removed key.
            let retained = state.by_id.len().saturating_mul(4).max(64);
            if state.by_id.capacity() > retained {
                state.by_id.shrink_to(retained);
                state.by_identity.shrink_to(retained);
            }
        }
    }
}

impl State {
    fn insert(&mut self, identity: LockRelationIdentity, permanent: bool) -> u64 {
        let table = self.next;
        self.next = self
            .next
            .checked_add(1)
            .expect("lock identity space exhausted");
        self.by_identity.insert(identity.clone(), table);
        self.by_id.insert(
            table,
            Record {
                identity,
                permanent,
                retained: Weak::new(),
            },
        );
        table
    }
}

impl IdentityRegistry {
    pub(super) fn permanent(&self, identity: LockRelationIdentity) -> u64 {
        let mut state = self.0.lock();
        if let Some(table) = state.by_identity.get(&identity).copied() {
            state
                .by_id
                .get_mut(&table)
                .expect("interned identity")
                .permanent = true;
            return table;
        }
        state.insert(identity, true)
    }

    pub(super) fn key(&self, digest: [u8; 32]) -> KeyReservationIdentity {
        let identity = LockRelationIdentity::KeyReservation(digest);
        let mut state = self.0.lock();
        let existing = state.by_identity.get(&identity).copied();
        let table = if let Some(table) = existing {
            let record = &state.by_id[&table];
            if let Some(retained) = record.retained.upgrade() {
                return KeyReservationIdentity(retained);
            }
            if record.permanent {
                table
            } else {
                // The last owner may be waiting for this mutex to finish its destructor.
                state.by_id.remove(&table);
                state.insert(identity, false)
            }
        } else {
            state.insert(identity, false)
        };
        let retained = Arc::new(Retained {
            table,
            registry: Arc::downgrade(&self.0),
        });
        state
            .by_id
            .get_mut(&table)
            .expect("interned identity")
            .retained = Arc::downgrade(&retained);
        KeyReservationIdentity(retained)
    }

    pub(super) fn retain(&self, table: u64) -> Option<KeyReservationIdentity> {
        self.0
            .lock()
            .by_id
            .get(&table)?
            .retained
            .upgrade()
            .map(KeyReservationIdentity)
    }

    pub(super) fn identity(&self, table: u64) -> Option<LockRelationIdentity> {
        self.0
            .lock()
            .by_id
            .get(&table)
            .map(|record| record.identity.clone())
    }

    #[cfg(test)]
    pub(super) fn retained_counts(&self) -> (usize, usize, usize, usize) {
        let state = self.0.lock();
        (
            state.by_id.len(),
            state.by_identity.len(),
            state.by_id.capacity(),
            state.by_identity.capacity(),
        )
    }
}

#[cfg(test)]
mod tests;
