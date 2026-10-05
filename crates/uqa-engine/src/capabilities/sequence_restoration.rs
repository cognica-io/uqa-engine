//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Lend sequence registry guards and retain the original concrete publication order.
use crate::Engine;
use uqa_execution::catalog::sequence::restoration::{
    RestoredSequenceRegistry, SequencePersistenceRead, SequenceRestoreContext,
    SequenceRestoreRegistry,
};
impl Engine {
    pub(crate) fn sequence_restore_context(&self) -> SequenceRestoreContext<'_> {
        SequenceRestoreContext {
            sequences: self,
            security: self,
            registry: self,
            roles: self,
        }
    }

    pub(crate) fn install_sequence_read_snapshot(
        &self,
        snapshot: &uqa_execution::catalog::sequence::snapshot::SequenceReadSnapshot,
    ) {
        self.durable.roles.restore(&snapshot.roles.roles);
        self.durable
            .role_memberships
            .restore(&snapshot.roles.memberships);
        self.durable.sequences.restore(&snapshot.sequences);
        self.durable
            .sequence_object_ids
            .restore(&snapshot.object_ids);
        self.durable
            .sequence_catalog_oids
            .restore(&snapshot.catalog_oids);
        self.durable
            .sequence_persistence
            .restore(&snapshot.persistence);
        self.durable.sequence_security.restore(&snapshot.security);
    }
}
impl SequenceRestoreRegistry for Engine {
    fn persistence(&self) -> SequencePersistenceRead<'_> {
        Box::new(self.durable.sequence_persistence.read())
    }
    fn install(&self, registry: RestoredSequenceRegistry) {
        *self.durable.sequences.write() = registry.sequences;
        *self.durable.sequence_object_ids.write() = registry.object_ids;
        *self.durable.sequence_persistence.write() = registry.persistence;
        *self.durable.sequence_security.write() = registry.security;
    }
}

impl crate::DurableCatalogSnapshot {
    pub(crate) fn sequence_read_snapshot(
        &self,
    ) -> uqa_execution::catalog::sequence::snapshot::SequenceReadSnapshot {
        uqa_execution::catalog::sequence::snapshot::SequenceReadSnapshot {
            sequences: self.sequences.clone(),
            object_ids: self.sequence_object_ids.clone(),
            persistence: self.sequence_persistence.clone(),
            security: self.sequence_security.clone(),
            catalog_oids: self.sequence_catalog_oids.clone(),
            roles: uqa_execution::catalog::security::roles::persistence::RoleCatalogSnapshot {
                roles: self.roles.clone(),
                memberships: self.role_memberships.clone(),
            },
        }
    }
}

impl uqa_execution::catalog::sequence::snapshot::SequenceSnapshotSource for Engine {
    fn sequence_read_snapshot(
        &self,
    ) -> uqa_storage::StorageBackendResult<
        uqa_execution::catalog::sequence::snapshot::SequenceReadSnapshot,
    > {
        self.latest_sequence_snapshot()
    }

    fn sequence_position(
        &self,
        key: crate::row_locks::SequencePositionKey,
    ) -> uqa_storage::StorageBackendResult<Option<crate::row_locks::RecordedSequencePosition>> {
        let Some(positions) = self.shared_sequence_positions() else {
            return Ok(None);
        };
        positions
            .sequence_position(key)
            .map_err(|error| uqa_storage::StorageBackendError::Other(error.to_string()))
    }
}

impl Engine {
    /// The sequences of `snapshot` at their exact positions, by qualified name.
    pub(crate) fn sequence_states_at_positions(
        &self,
        snapshot: &uqa_execution::catalog::sequence::snapshot::SequenceReadSnapshot,
    ) -> uqa_storage::StorageBackendResult<
        std::collections::BTreeMap<uqa_core::RelationIdentity, crate::SequenceState>,
    > {
        let Some(positions) = self.shared_sequence_positions() else {
            return Ok((*snapshot.sequences).clone());
        };
        let positions = positions
            .sequence_positions()
            .map_err(|error| uqa_storage::StorageBackendError::Other(error.to_string()))?;
        Ok(snapshot
            .sequences
            .iter()
            .map(|(relation, state)| {
                let position = snapshot
                    .object_ids
                    .get(relation)
                    .and_then(|object_id| positions.get(&state.position_key(*object_id)));
                (relation.clone(), state.at_position(position.copied()))
            })
            .collect())
    }

    /// Public sequence metadata retains an attached or query-owned catalog; ordinary value operations use the independent sequence source.
    pub(crate) fn query_sequence_snapshot(
        &self,
    ) -> uqa_storage::StorageBackendResult<
        uqa_execution::catalog::sequence::snapshot::SequenceReadSnapshot,
    > {
        if let Some(snapshot) = self.query_catalog_snapshot.as_ref() {
            return Ok(snapshot.sequence_read_snapshot());
        }
        let attached = self.transaction_depth() == 0
            && self
                .storage
                .backend
                .as_ref()
                .is_some_and(|backend| backend.in_transaction());
        let independent = if attached {
            self.synchronize_catalog_registries()?;
            None
        } else {
            self.open_independent_catalog_session(None)?
        };
        self.sequence_snapshot_from_catalog(independent.as_ref())
    }

    fn sequence_snapshot_from_catalog(
        &self,
        independent: Option<&uqa_storage::PersistentStorageSession>,
    ) -> uqa_storage::StorageBackendResult<
        uqa_execution::catalog::sequence::snapshot::SequenceReadSnapshot,
    > {
        uqa_execution::catalog::sequence::snapshot::read_sequence_snapshot(
            self.sequence_registries(),
            self.storage.catalog.as_deref(),
            independent,
            self.versioned_backend_transactions(),
        )
    }
}
