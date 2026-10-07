//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Exact relation identities leased across holders and waiters. The native pin prevents slot reuse until the last process using that identity releases it.

use super::super::super::{relation_byte_claims, RelationLockMode, RELATION_BASE, RELATION_SPAN};
use super::super::{CoordinatorState, FileLockCoordinator};
use super::{Admission, RELATION_ADMISSION_BYTE};
use rusqlite::OptionalExtension;
use std::collections::HashMap;
use uqa_sql::SQLError;

const ATTACHMENT_BYTE: u64 = 15;

struct Pin {
    slot: u64,
    users: usize,
    poisoned: bool,
}

#[derive(Default)]
pub(in crate::row_locks::cross_process::file) struct Identities {
    connection: Option<rusqlite::Connection>,
    pins: HashMap<Vec<u8>, Pin>,
    slots: HashMap<u64, u64>,
}

pub(in crate::row_locks) struct RelationIdentityLease<'a> {
    coordinator: &'a FileLockCoordinator,
    relation: &'a [u8],
    slot: u64,
    generation: u64,
    retained: bool,
}

impl RelationIdentityLease<'_> {
    pub(in crate::row_locks) fn generation(&self) -> u64 {
        self.generation
    }

    pub(in crate::row_locks) fn slot(&self) -> u64 {
        self.slot
    }
    pub(in crate::row_locks) fn retain(&mut self) {
        self.retained = true;
    }
}

impl Drop for RelationIdentityLease<'_> {
    fn drop(&mut self) {
        if !self.retained {
            self.coordinator
                .unpin_relation(&mut self.coordinator.state.lock(), self.relation);
        }
    }
}

fn sql(error: rusqlite::Error) -> String {
    format!("relation identity registry: {error}")
}

impl FileLockCoordinator {
    pub(in crate::row_locks::cross_process::file) fn attach_relation_registry(
        &self,
    ) -> Result<(), String> {
        let mut state = self.state.lock();
        uqa_storage::native_file::lock_byte(&self.file, RELATION_ADMISSION_BYTE, true)
            .map_err(|error| format!("admit relation registry: {error}"))?;
        let mut admission = Admission {
            coordinator: self,
            active: true,
        };
        self.open_relation_identities(&mut state)?;
        admission.release()
    }

    pub(in crate::row_locks) fn pin_relation<'a>(
        &'a self,
        relation: &'a [u8],
        cancel: &uqa_core::CancellationToken,
    ) -> Result<RelationIdentityLease<'a>, SQLError> {
        loop {
            cancel.check()?;
            if let Some(slot) = self
                .try_pin_relation(relation)
                .map_err(SQLError::Internal)?
            {
                return Ok(RelationIdentityLease {
                    coordinator: self,
                    relation,
                    slot,
                    generation: self.state.lock().relation_identities.slots[&slot],
                    retained: false,
                });
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
    }

    pub(super) fn try_pin_relation(&self, relation: &[u8]) -> Result<Option<u64>, String> {
        let mut state = self.state.lock();
        if let Some(pin) = state.relation_identities.pins.get_mut(relation) {
            pin.users += 1;
            return Ok(Some(pin.slot));
        }
        if let Err(error) = self.apply_byte_mode(RELATION_ADMISSION_BYTE, None, Some(true)) {
            return if super::lock_would_block(&error) {
                Ok(None)
            } else {
                Err(format!("admit relation identity: {error}"))
            };
        }
        let mut admission = Admission {
            coordinator: self,
            active: true,
        };
        let slot = self.pin_admitted_relation(&mut state, relation)?;
        if let Err(error) = admission.release() {
            self.unpin_relation(&mut state, relation);
            return Err(error);
        }
        Ok(Some(slot))
    }

    fn pin_admitted_relation(
        &self,
        state: &mut CoordinatorState,
        relation: &[u8],
    ) -> Result<u64, String> {
        self.open_relation_identities(state)?;
        let identities = &mut state.relation_identities;
        let connection = identities
            .connection
            .as_ref()
            .expect("identity registry opened");
        let existing = connection
            .query_row(
                "SELECT slot,generation FROM identities WHERE identity=?1",
                [relation],
                |row| Ok((u64::from(row.get::<_, u32>(0)?), row.get::<_, i64>(1)?)),
            )
            .optional()
            .map_err(sql)?;
        let (slot, generation) = match existing {
            Some(identity) => identity,
            None => {
                let mut statement = connection
                    .prepare("SELECT slot FROM identities ORDER BY slot")
                    .map_err(sql)?;
                let mut rows = statement.query([]).map_err(sql)?;
                let mut available = None;
                let mut next = 0;
                while let Some(row) = rows.next().map_err(sql)? {
                    let slot = u64::from(row.get::<_, u32>(0).map_err(sql)?);
                    if slot >= RELATION_SPAN {
                        return Err("invalid relation identity slot".into());
                    }
                    next = slot + 1;
                    if identities.slots.contains_key(&slot) {
                        continue;
                    }
                    match self.apply_byte_mode(RELATION_BASE + slot, None, Some(true)) {
                        Ok(()) => {
                            self.apply_byte_mode(RELATION_BASE + slot, Some(true), None)
                                .map_err(|error| {
                                    format!("release relation identity probe: {error}")
                                })?;
                            available = Some(slot);
                            break;
                        }
                        Err(error) if super::lock_would_block(&error) => {}
                        Err(error) => return Err(format!("probe relation identity: {error}")),
                    }
                }
                drop(rows);
                drop(statement);
                let slot = available.unwrap_or(next);
                if slot >= RELATION_SPAN {
                    return Err("too many simultaneously retained relation identities".into());
                }
                let transaction = connection.unchecked_transaction().map_err(sql)?;
                let generation: i64 = transaction.query_row("UPDATE generation SET value=value+1 WHERE value<9223372036854775807 RETURNING value", [], |row| row.get(0)).map_err(sql)?;
                transaction.execute("INSERT INTO identities(slot,identity,generation) VALUES(?1,?2,?3) ON CONFLICT(slot) DO UPDATE SET identity=excluded.identity,generation=excluded.generation", rusqlite::params![u32::try_from(slot).expect("bounded relation slot"),relation,generation]).map_err(sql)?;
                transaction.commit().map_err(sql)?;
                (slot, generation)
            }
        };
        if slot >= RELATION_SPAN {
            return Err("invalid relation identity slot".into());
        }
        let generation = u64::try_from(generation)
            .ok()
            .filter(|value| *value != 0)
            .ok_or("invalid relation identity generation")?;
        self.apply_byte_mode(RELATION_BASE + slot, None, Some(false))
            .map_err(|error| format!("pin relation identity: {error}"))?;
        identities.pins.insert(
            relation.to_vec(),
            Pin {
                slot,
                users: 1,
                poisoned: false,
            },
        );
        identities.slots.insert(slot, generation);
        Ok(slot)
    }

    pub(super) fn unpin_relation(&self, state: &mut CoordinatorState, relation: &[u8]) {
        let identities = &mut state.relation_identities;
        let Some(pin) = identities.pins.get_mut(relation) else {
            return;
        };
        pin.users -= 1;
        if pin.users == 0 && !pin.poisoned {
            // An unlock error must leave the mapping pinned locally, so another identity can never reuse a possibly held native byte.
            if self
                .apply_byte_mode(RELATION_BASE + pin.slot, Some(false), None)
                .is_ok()
            {
                identities.slots.remove(&pin.slot);
                identities.pins.remove(relation);
            }
        }
    }

    pub(in crate::row_locks) fn release_relation(
        &self,
        session: u64,
        relation: &[u8],
        mode: RelationLockMode,
    ) {
        let mut state = self.state.lock();
        if let Some(pin) = state.relation_identities.pins.get(relation) {
            for claim in relation_byte_claims(pin.slot, mode) {
                self.release_one(&mut state, session, claim);
            }
            self.unpin_relation(&mut state, relation);
        }
    }

    pub(in crate::row_locks) fn retained_row_identity(
        &self,
        relation: &[u8],
        doc_id: u64,
    ) -> Option<super::super::super::RowIdentity> {
        use super::super::super::RowIdentity;
        if let Some(identity) = RowIdentity::key(relation, doc_id) {
            return Some(identity);
        }
        let state = self.state.lock();
        let identities = &state.relation_identities;
        let slot = identities.pins.get(relation)?.slot;
        Some(RowIdentity::Relation {
            generation: *identities.slots.get(&slot)?,
            doc_id,
        })
    }

    pub(in crate::row_locks) fn release_row_identity(&self, relation: &[u8]) {
        if super::super::super::RowIdentity::key(relation, 0).is_none() {
            self.unpin_relation(&mut self.state.lock(), relation);
        }
    }

    fn open_relation_identities(&self, state: &mut CoordinatorState) -> Result<(), String> {
        if state.relation_identities.connection.is_some() {
            return Ok(());
        }
        let first = match self.apply_byte_mode(ATTACHMENT_BYTE, None, Some(true)) {
            Ok(()) => true,
            Err(error) if super::lock_would_block(&error) => false,
            Err(error) => return Err(format!("attach relation identities: {error}")),
        };
        let opened = self.open_relation_registry(first);
        let connection = match opened {
            Ok(connection) => connection,
            Err(error) => {
                if first {
                    let _ = self.apply_byte_mode(ATTACHMENT_BYTE, Some(true), None);
                }
                return Err(error);
            }
        };
        if let Err(error) =
            self.apply_byte_mode(ATTACHMENT_BYTE, first.then_some(true), Some(false))
        {
            drop(connection);
            if first {
                let _ = self.apply_byte_mode(ATTACHMENT_BYTE, Some(true), None);
            }
            return Err(format!("retain relation registry attachment: {error}"));
        }
        state.relation_identities.connection = Some(connection);
        Ok(())
    }

    fn open_relation_registry(&self, first: bool) -> Result<rusqlite::Connection, String> {
        if first {
            self.clear_relation_epoch_metadata()?;
            // No process retains a holder or waiter from the old epoch. This sidecar has no committed data; rebuilding also recovers it after a host failure with synchronous=OFF.
            for suffix in ["", "-journal"] {
                let mut path = self.relation_path.as_os_str().to_owned();
                path.push(suffix);
                match std::fs::remove_file(std::path::Path::new(&path)) {
                    Ok(()) => {}
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                    Err(error) => return Err(format!("reset relation registry epoch: {error}")),
                }
            }
        }
        let connection = rusqlite::Connection::open(&self.relation_path).map_err(sql)?;
        if let Some(key) = &self.relation_key {
            connection
                .pragma_update(None, "key", key.expose_secret())
                .map_err(sql)?;
            let cipher: String = connection
                .pragma_query_value(None, "cipher_version", |row| row.get(0))
                .map_err(sql)?;
            if cipher.is_empty() {
                return Err("encrypted relation registry requires SQLCipher".into());
            }
        }
        // Read before configuring or writing: an incorrect credential must not mutate another live process's registry.
        let version: u32 = connection
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .map_err(sql)?;
        if (!first && version != 2) || (first && version != 0) {
            return Err("invalid relation registry version".into());
        }
        connection.execute_batch("PRAGMA synchronous=OFF; PRAGMA temp_store=MEMORY; PRAGMA cache_size=-64; PRAGMA secure_delete=ON;").map_err(sql)?;
        if first {
            connection.execute_batch("CREATE TABLE generation(value INTEGER NOT NULL); INSERT INTO generation VALUES(0); CREATE TABLE identities(slot INTEGER PRIMARY KEY CHECK(slot>=0 AND slot<1048576), identity BLOB NOT NULL UNIQUE, generation INTEGER NOT NULL CHECK(generation>0)); PRAGMA user_version=2;").map_err(sql)?;
        }
        Ok(connection)
    }

    pub(in crate::row_locks::cross_process::file) fn close_relation_identities(&self) {
        // Close SQLite before the native descriptor releases the attachment and every pin together. A new epoch cannot start while this connection is closing.
        self.state.lock().relation_identities.connection.take();
    }

    pub(in crate::row_locks::cross_process::file) fn local_relation_generation(
        state: &CoordinatorState,
        offset: u64,
    ) -> u64 {
        super::super::super::relation_slot_of_claim(offset)
            .and_then(|slot| state.relation_identities.slots.get(&slot).copied())
            .unwrap_or(0)
    }

    pub(in crate::row_locks::cross_process::file) fn current_relation_generation(
        &self,
        offset: u64,
    ) -> Option<u64> {
        let Some(slot) = super::super::super::relation_slot_of_claim(offset) else {
            return Some(0);
        };
        let mut state = self.state.lock();
        if let Some(generation) = state.relation_identities.slots.get(&slot) {
            return Some(*generation);
        }
        if state.relation_identities.connection.is_none() {
            // A row-only waiter may follow a foreign relation edge. Attach lazily so its graph traversal uses the same registry even before it acquires any relation of its own.
            self.apply_byte_mode(RELATION_ADMISSION_BYTE, None, Some(true))
                .ok()?;
            let mut admission = Admission {
                coordinator: self,
                active: true,
            };
            self.open_relation_identities(&mut state).ok()?;
            admission.release().ok()?;
        }
        let generation: i64 = state
            .relation_identities
            .connection
            .as_ref()?
            .query_row(
                "SELECT generation FROM identities WHERE slot=?1",
                [u32::try_from(slot).ok()?],
                |row| row.get(0),
            )
            .ok()?;
        u64::try_from(generation)
            .ok()
            .filter(|generation| *generation != 0)
    }

    pub(in crate::row_locks::cross_process::file) fn poison_relation_slot(
        state: &mut CoordinatorState,
        offset: u64,
    ) {
        if let Some(slot) = super::super::super::relation_slot_of_claim(offset) {
            for pin in state.relation_identities.pins.values_mut() {
                if pin.slot == slot {
                    pin.poisoned = true;
                }
            }
        }
    }

    #[cfg(test)]
    pub(in crate::row_locks::cross_process::file) fn relation_registry_counts(
        &self,
    ) -> (usize, i64) {
        let state = self.state.lock();
        let identities = &state.relation_identities;
        let count = identities
            .connection
            .as_ref()
            .unwrap()
            .query_row("SELECT count(*) FROM identities", [], |row| row.get(0))
            .unwrap();
        (identities.pins.len(), count)
    }

    #[cfg(test)]
    pub(in crate::row_locks) fn relation_slot(&self, relation: &[u8]) -> u64 {
        self.pin_relation(relation, &uqa_core::CancellationToken::new())
            .unwrap()
            .slot()
    }
}
