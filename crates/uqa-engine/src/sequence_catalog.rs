//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bind durable sequence loading and registry publication to Engine state.

use super::{
    CatalogFacade, Engine, RelationIdentity, SequenceRow, SequenceState, StorageBackendResult,
};
use crate::state::SequenceSecurity;

impl Engine {
    pub(crate) fn sequence_row(
        name: &str,
        object_id: [u8; 16],
        state: SequenceState,
        persistence: uqa_sql::ast::RelationPersistence,
        security: &SequenceSecurity,
    ) -> StorageBackendResult<SequenceRow> {
        uqa_execution::catalog::sequence::sequence_row(
            name,
            object_id,
            state,
            persistence,
            security,
        )
    }

    pub(crate) fn refresh_sequences_from_catalog(&self) -> StorageBackendResult<()> {
        let sequence_session = self.open_nontransactional_sequence_session()?;
        let catalog = sequence_session
            .as_ref()
            .map(|session| session.catalog.as_ref())
            .or(self.storage.catalog.as_deref());
        let Some(catalog) = catalog else {
            return Ok(());
        };
        let rows = catalog.load_sequence_rows()?;
        self.install_durable_sequence_rows(rows)?;
        Ok(())
    }

    /// Restore the typed sequence registry without modifying the catalog.
    /// This is safe for initial hydration, pinned snapshots, external-commit
    /// refreshes, and rollback cleanup alike.
    pub(crate) fn restore_sequences_from_catalog(
        &self,
        catalog: &dyn CatalogFacade,
    ) -> StorageBackendResult<()> {
        let rows = catalog.load_sequence_rows()?;
        self.install_durable_sequence_rows(rows)?;
        Ok(())
    }

    fn install_durable_sequence_rows(&self, rows: Vec<SequenceRow>) -> StorageBackendResult<()> {
        uqa_execution::catalog::sequence::restoration::restore_sequence_rows(
            &self.sequence_restore_context(),
            rows,
        )
    }
}

impl Engine {
    pub(crate) fn move_sequence_state(
        &self,
        source: &RelationIdentity,
        target: &RelationIdentity,
    ) -> Result<(), crate::SQLError> {
        let mut sequences = self.durable.sequences.write();
        let mut object_ids = self.durable.sequence_object_ids.write();
        let mut persistence = self.durable.sequence_persistence.write();
        let mut security = self.durable.sequence_security.write();
        if !sequences.contains_key(source)
            || !object_ids.contains_key(source)
            || !persistence.contains_key(source)
            || !security.contains_key(source)
        {
            return Err(crate::SQLError::Internal(format!(
                "sequence registry entry `{}` disappeared during rename",
                source.qualified_name()
            )));
        }
        if sequences.contains_key(target)
            || object_ids.contains_key(target)
            || persistence.contains_key(target)
            || security.contains_key(target)
        {
            return Err(crate::SQLError::Internal(format!(
                "sequence registry target `{}` appeared during rename",
                target.qualified_name()
            )));
        }
        let state = sequences
            .remove(source)
            .expect("preflighted sequence state must exist");
        let object_id = object_ids
            .remove(source)
            .expect("preflighted sequence object identity must exist");
        let stored_persistence = persistence
            .remove(source)
            .expect("preflighted sequence persistence must exist");
        let stored_security = security
            .remove(source)
            .expect("preflighted sequence security must exist");
        sequences.insert(target.clone(), state);
        object_ids.insert(target.clone(), object_id);
        persistence.insert(target.clone(), stored_persistence);
        security.insert(target.clone(), stored_security);
        Ok(())
    }
}
