//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! The sequence catalog a session last read from the latest commit, kept while nothing it was read from has changed.
//!
//! Sequence values resolve names, definitions and authority against the latest commit and not against the caller's statement snapshot, and one inserted row asks for them several times. A read through an independent session costs more than everything else the row needs, so the session keeps the last read with what it depended on: the commits of the database, its own private records and its registries. While all three are unchanged, a new read returns the same catalog.

use std::sync::Arc;

use uqa_execution::catalog::security::roles::persistence::RoleCatalogSnapshot;
use uqa_execution::catalog::sequence::snapshot::{read_sequence_snapshot, SequenceReadSnapshot};
use uqa_storage::key_value::KeyValueReadRevision;
use uqa_storage::StorageBackendResult;

use crate::Engine;

/// What a read of the sequence catalog depends on beside the session's registries.
struct SequenceSnapshotSources {
    /// Changes with every commit of any session or process.
    commits: u64,
    /// The session's record view, whose private records the read merges.
    view: KeyValueReadRevision,
}

impl SequenceSnapshotSources {
    fn unchanged(&self, later: &Self) -> bool {
        self.commits == later.commits && self.view.same_private_changes(&later.view)
    }
}

pub(crate) struct SequenceSnapshotMemo {
    sources: SequenceSnapshotSources,
    /// The registries the read started from, which supply temporary sequences and private roles.
    registries: SequenceReadSnapshot,
    snapshot: SequenceReadSnapshot,
}

fn same_registries(left: &SequenceReadSnapshot, right: &SequenceReadSnapshot) -> bool {
    Arc::ptr_eq(&left.sequences, &right.sequences)
        && Arc::ptr_eq(&left.object_ids, &right.object_ids)
        && Arc::ptr_eq(&left.persistence, &right.persistence)
        && Arc::ptr_eq(&left.security, &right.security)
        && Arc::ptr_eq(&left.roles.roles, &right.roles.roles)
        && Arc::ptr_eq(&left.roles.memberships, &right.roles.memberships)
}

impl SequenceSnapshotMemo {
    /// The remembered read, when a read from `registries` would now return the same. That holds for the registries the read started from and for the ones it returned, which a refresh installs: a read takes only temporary sequences and private roles from them, and its result holds both.
    fn reuse(
        &self,
        sources: &SequenceSnapshotSources,
        registries: &SequenceReadSnapshot,
    ) -> Option<SequenceReadSnapshot> {
        (self.sources.unchanged(sources)
            && (same_registries(&self.registries, registries)
                || same_registries(&self.snapshot, registries)))
        .then(|| self.snapshot.clone())
    }
}

impl Engine {
    /// The sequence registries of this session, as a read of the catalog starts from them.
    pub(crate) fn sequence_registries(&self) -> SequenceReadSnapshot {
        SequenceReadSnapshot {
            sequences: self.durable.sequences.snapshot(),
            object_ids: self.durable.sequence_object_ids.snapshot(),
            persistence: self.durable.sequence_persistence.snapshot(),
            security: self.durable.sequence_security.snapshot(),
            catalog_oids: self.durable.sequence_catalog_oids.snapshot(),
            roles: RoleCatalogSnapshot {
                roles: self.durable.roles.snapshot(),
                memberships: self.durable.role_memberships.snapshot(),
            },
        }
    }

    /// `None` when the backend cannot tell that nothing was committed, or the session's record view has no identity; every read then goes to the catalog.
    fn sequence_snapshot_sources(&self) -> StorageBackendResult<Option<SequenceSnapshotSources>> {
        let Some(backend) = self.storage.backend.as_ref() else {
            return Ok(None);
        };
        if !self.versioned_backend_transactions() {
            return Ok(None);
        }
        let Some(commits) = backend.commit_monitor_version()? else {
            return Ok(None);
        };
        Ok(backend
            .read_view_revision()?
            .map(|view| SequenceSnapshotSources { commits, view }))
    }

    /// Sequence definitions and authority as the latest commit holds them, with this session's private records and temporary sequences.
    pub(crate) fn latest_sequence_snapshot(&self) -> StorageBackendResult<SequenceReadSnapshot> {
        let registries = self.sequence_registries();
        let sources = self.sequence_snapshot_sources()?;
        if let Some(sources) = sources.as_ref() {
            let remembered = self
                .session
                .sequence_snapshot
                .lock()
                .as_ref()
                .and_then(|memo| memo.reuse(sources, &registries));
            if let Some(snapshot) = remembered {
                return Ok(snapshot);
            }
        }
        #[cfg(test)]
        self.session
            .sequence_snapshot_reads
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let session = self.open_independent_catalog_session(None)?;
        let snapshot = read_sequence_snapshot(
            registries.clone(),
            self.storage.catalog.as_deref(),
            session.as_ref(),
            self.versioned_backend_transactions(),
        )?;
        drop(session);
        // A commit during the read leaves unknown which side of it the read saw, so only a read without one is kept.
        let kept = match (sources, self.sequence_snapshot_sources()?) {
            (Some(before), Some(after)) if before.unchanged(&after) => Some(SequenceSnapshotMemo {
                sources: before,
                registries,
                snapshot: snapshot.clone(),
            }),
            _ => None,
        };
        *self.session.sequence_snapshot.lock() = kept;
        Ok(snapshot)
    }
}
