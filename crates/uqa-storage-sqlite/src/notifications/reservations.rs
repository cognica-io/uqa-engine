//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Retain one unresolved publication range without retaining a physical registry writer.

use super::{
    publication::state, registry_error, NotificationRegistry, NotificationRegistryTransaction,
    StorageBackendError, StorageBackendResult, REGISTRY_BUSY_TIMEOUT,
};
use rusqlite::params;
use std::time::{Duration, Instant};
use uqa_storage::{
    notifications::{NotificationPublication, NotificationPublicationStore},
    read_control::StorageReadControl,
};

impl NotificationRegistry {
    /// Publishers wait for the original reserved range; consumers and recovery can continue using ordinary registry transactions. Owner liveness comes from a retained process lease.
    pub fn begin_publishing(
        &self,
        store: &dyn NotificationPublicationStore,
        control: &StorageReadControl,
        resume: Option<(&NotificationPublication, [u8; 16])>,
        owner_alive: &mut dyn FnMut([u8; 16]) -> StorageBackendResult<bool>,
    ) -> StorageBackendResult<NotificationRegistryTransaction> {
        let started = Instant::now();
        loop {
            control.check()?;
            let mut transaction = self.begin_recovered(store, control)?;
            let current = state(&transaction.connection)?;
            if resume.is_some_and(|(publication, _)| {
                current.registry_id != publication.view().header().registry_id
            }) {
                return Err(StorageBackendError::Other(
                    "retained notification belongs to a different registry incarnation".into(),
                ));
            }
            let already_applied = resume.is_some_and(|(publication, _)| {
                let header = publication.view().header();
                current.next_publication > header.publication_sequence
            });
            let resume_identity =
                resume.map(|(publication, owner)| (publication.fingerprint(), owner));
            let pending = current
                .reservation
                .filter(|pending| !already_applied && Some(*pending) != resume_identity);
            if let Some((fingerprint, owner)) = pending {
                if owner_alive(owner)? {
                    drop(transaction);
                    if started.elapsed() >= REGISTRY_BUSY_TIMEOUT {
                        return Err(registry_error(
                            "wait for unresolved notification publication",
                            &rusqlite::Error::SqliteFailure(
                                rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_BUSY),
                                None,
                            ),
                        ));
                    }
                    std::thread::park_timeout(Duration::from_millis(10));
                    continue;
                }
                transaction.connection.execute(
                    "UPDATE publication_state SET reserved_fingerprint = NULL, reservation_owner = NULL WHERE singleton = 1 AND reserved_fingerprint = ?1 AND reservation_owner = ?2",
                    params![fingerprint.as_slice(), owner.as_slice()],
                ).map_err(|error| registry_error("discard abandoned publication reservation", &error))?;
            }
            transaction
                .connection
                .execute_batch("SAVEPOINT notification_preparation")
                .map_err(|error| {
                    registry_error("retain notification preparation boundary", &error)
                })?;
            transaction.preparing = true;
            return Ok(transaction);
        }
    }
}

impl NotificationRegistryTransaction {
    /// Roll back the private queue append and persist only its reservation. The sender keeps the matching publication and liveness lease until its authoritative main outcome is resolved.
    pub fn suspend_publication(
        &mut self,
        publication: &NotificationPublication,
        owner: [u8; 16],
    ) -> StorageBackendResult<()> {
        if !self.preparing || self.finished || self.poisoned {
            return Err(StorageBackendError::Other(
                "notification publication has no resumable preparation boundary".into(),
            ));
        }
        self.connection
            .execute_batch("ROLLBACK TO notification_preparation; RELEASE notification_preparation")
            .map_err(|error| registry_error("restore reserved notification boundary", &error))?;
        self.preparing = false;
        let current = state(&self.connection)?;
        let header = publication.view().header();
        let fingerprint = publication.fingerprint();
        if current.registry_id != header.registry_id
            || current.next_publication != header.publication_sequence
            || current
                .reservation
                .is_some_and(|pending| pending != (fingerprint, owner))
        {
            self.poisoned = true;
            return Err(StorageBackendError::Other(
                "notification reservation no longer matches its original publication".into(),
            ));
        }
        self.connection.execute(
            "UPDATE publication_state SET reserved_fingerprint = ?1, reservation_owner = ?2 WHERE singleton = 1",
            params![fingerprint.as_slice(), owner.as_slice()],
        ).map_err(|error| registry_error("retain unresolved notification publication", &error))?;
        self.connection.execute_batch("COMMIT").map_err(|error| {
            registry_error("commit notification publication reservation", &error)
        })?;
        self.finished = true;
        Ok(())
    }

    /// Return true when an independent recovery already applied this original main commit. Later publications may have advanced the single acknowledgement since then.
    pub fn resume_publication(
        &mut self,
        publication: &NotificationPublication,
        owner: [u8; 16],
        control: &StorageReadControl,
    ) -> StorageBackendResult<bool> {
        let current = state(&self.connection)?;
        let header = publication.view().header();
        if current.registry_id != header.registry_id {
            return Err(StorageBackendError::Other(
                "retained notification belongs to a different registry incarnation".into(),
            ));
        }
        if current.next_publication > header.publication_sequence {
            return Ok(true);
        }
        if current.reservation != Some((publication.fingerprint(), owner)) {
            return Err(StorageBackendError::Other(
                "retained notification has no matching publication reservation".into(),
            ));
        }
        self.apply_publication(publication.view(), control)?;
        Ok(false)
    }
}
