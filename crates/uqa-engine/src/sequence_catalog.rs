//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bind durable sequence loading and registry publication to Engine state.

use super::{CatalogFacade, Engine, RelationIdentity, StorageBackendResult};
use uqa_execution::catalog::sequence::snapshot::SequenceSnapshotSource;

impl Engine {
    pub(crate) fn refresh_sequences_from_catalog(&self) -> StorageBackendResult<()> {
        let snapshot = self.sequence_read_snapshot()?;
        self.install_sequence_read_snapshot(&snapshot);
        Ok(())
    }

    /// Restore sequence state and authority inside the caller's catalog transaction.
    pub(crate) fn restore_sequences_from_catalog(
        &self,
        catalog: &dyn CatalogFacade,
        allow_migration: bool,
    ) -> StorageBackendResult<()> {
        uqa_execution::catalog::sequence::restoration::restore_sequence_catalog(
            &self.sequence_restore_context(),
            catalog,
            allow_migration,
        )
    }
}

impl Engine {
    pub(crate) fn move_sequence_state(
        &self,
        source: &RelationIdentity,
        target: &RelationIdentity,
    ) -> Result<(), crate::SQLError> {
        uqa_execution::catalog::sequence::registry::publication::rename(
            &mut self.durable.sequences.write(),
            self.storage.catalog.as_deref(),
            source,
            target,
        )
    }
}
