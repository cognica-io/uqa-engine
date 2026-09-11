//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{
    BTreeMap, Engine, RelationIdentity, SQLError, SequenceDataType, SequenceRestart, SequenceState,
    StorageBackendError, StorageBackendResult,
};

mod dependencies;

pub(crate) use dependencies::SequenceSchemaDependent;

impl Engine {
    /// Resolve a sequence reference at DDL binding time using the current
    /// `search_path`. Persisted expressions must store the returned canonical
    /// relation name so later session state cannot change their target.
    pub(crate) fn resolve_sequence_reference_for_binding(
        &self,
        reference: &str,
    ) -> StorageBackendResult<String> {
        self.try_resolve_sequence_name(reference)?.ok_or_else(|| {
            StorageBackendError::Other(format!("Sequence `{reference}` does not exist"))
        })
    }

    /// Bind a session portal to the sequence's stable `regclass` carrier so a later rename or schema move cannot retarget or break the cursor.
    pub(crate) fn try_resolve_sequence_oid_reference_for_binding(
        &self,
        reference: &str,
    ) -> StorageBackendResult<Option<String>> {
        let Some(canonical) = self.try_resolve_sequence_name(reference)? else {
            return Ok(None);
        };
        let relation = Self::resolved_relation_identity(&canonical)?;
        let object_id = self
            .durable
            .sequence_object_ids
            .read()
            .get(&relation)
            .copied()
            .ok_or_else(|| {
                StorageBackendError::Other(format!(
                    "sequence `{canonical}` has no durable object identity"
                ))
            })?;
        Ok(Some(
            uqa_execution::catalog::projection::sequence_relation_oid(object_id).to_string(),
        ))
    }

    /// Resolve a reference read from persisted metadata against the loaded registry without using the current session's `search_path`. An unqualified local name is safe only when exactly one catalog sequence has that name.
    pub(crate) fn resolve_stored_sequence_reference_from_loaded_registry(
        &self,
        reference: &str,
    ) -> StorageBackendResult<String> {
        uqa_sql::schema::sequences::names::resolve_stored_sequence_reference(self, reference)
            .map_err(StorageBackendError::Other)
    }

    pub fn create_sequence(
        &self,
        name: &str,
        start: i64,
        increment: i64,
        if_not_exists: bool,
    ) -> Result<bool, String> {
        self.with_implicit_string_transaction(|engine| {
            uqa_execution::schema::sequences::creation::create_sequence(
                &engine.sequence_creation_context(),
                name,
                SequenceState::initial(start, increment, SequenceDataType::BigInt),
                if_not_exists,
                uqa_sql::ast::RelationPersistence::Permanent,
                &uqa_sql::ast::SequenceOwnership::Unchanged,
            )
            .map_err(|error| error.to_string())
        })
    }

    pub(crate) fn create_implicit_sequence_with_persistence(
        &self,
        name: &str,
        start: i64,
        increment: i64,
        data_type: SequenceDataType,
        persistence: uqa_sql::ast::RelationPersistence,
    ) -> Result<(), SQLError> {
        self.with_implicit_transaction(|engine| {
            uqa_execution::schema::sequences::creation::create_sequence(
                &engine.sequence_creation_context(),
                name,
                SequenceState::initial(start, increment, data_type),
                false,
                persistence,
                &uqa_sql::ast::SequenceOwnership::Unchanged,
            )?;
            Ok(())
        })
    }

    /// Compatibility wrapper for the original direct API. SQL lowering and
    /// all internal execution use [`SequenceRestart`] instead.
    #[allow(clippy::option_option)]
    pub fn alter_sequence(
        &self,
        name: &str,
        restart: Option<Option<i64>>,
        increment: Option<i64>,
        start: Option<i64>,
    ) -> Result<(), String> {
        let restart = match restart {
            None => SequenceRestart::Unchanged,
            Some(None) => SequenceRestart::FromStart,
            Some(Some(value)) => SequenceRestart::With(value),
        };
        let alter = uqa_sql::ast::AlterSequence {
            name: name.into(),
            restart,
            increment,
            start,
            ..Default::default()
        };
        self.with_implicit_string_transaction(|engine| {
            engine
                .alter_sequence_inner(&alter)
                .map(|_| ())
                .map_err(|error| error.to_string())
        })
    }

    fn alter_sequence_inner(&self, alter: &uqa_sql::ast::AlterSequence) -> Result<bool, SQLError> {
        uqa_execution::schema::sequences::dispatch::alter_sequence(
            &self.sequence_alter_context(),
            alter,
        )
    }

    pub(crate) fn persist_sequence_state_replacement(
        &self,
        name: &str,
        relation: &RelationIdentity,
        object_id: [u8; 16],
        persistence: uqa_sql::ast::RelationPersistence,
        state: SequenceState,
        invalidate_current_cache: bool,
    ) -> Result<(), SQLError> {
        let temporary = persistence == uqa_sql::ast::RelationPersistence::Temporary;
        let security = self
            .durable
            .sequence_security
            .read()
            .get(relation)
            .cloned()
            .ok_or_else(|| {
                SQLError::Internal(format!("sequence `{name}` has no security metadata"))
            })?;
        if !temporary {
            if let Some(catalog) = self.storage.catalog.as_ref() {
                if !catalog
                    .replace_sequence_row(
                        &Self::sequence_row(name, object_id, state, persistence, &security)
                            .map_err(|error| {
                                SQLError::Internal(format!("build sequence catalog row: {error}"))
                            })?,
                    )
                    .map_err(|error| {
                        SQLError::Internal(format!("persist sequence catalog: {error}"))
                    })?
                {
                    return Err(SQLError::Internal(format!(
                        "sequence `{name}` disappeared during ALTER"
                    )));
                }
            }
        }
        self.durable
            .sequences
            .write()
            .insert(relation.clone(), state);
        self.durable
            .sequence_persistence
            .write()
            .insert(relation.clone(), persistence);
        if invalidate_current_cache {
            self.session.sequence_caches.lock().remove(relation);
        }
        self.note_catalog_registry_changed();
        Ok(())
    }

    pub(crate) fn restart_owned_sequence(&self, name: &str) -> StorageBackendResult<()> {
        self.alter_sequence_inner(&uqa_sql::ast::AlterSequence {
            name: name.into(),
            restart: SequenceRestart::FromStart,
            ..Default::default()
        })
        .map(|_| ())
        .map_err(|error| StorageBackendError::Other(error.to_string()))
    }

    /// Snapshot of all registered sequences as `(name, state)` pairs.
    pub fn try_sequences_snapshot(&self) -> StorageBackendResult<BTreeMap<String, SequenceState>> {
        self.refresh_sequences_from_catalog()?;
        Ok(self
            .durable
            .sequences
            .read()
            .iter()
            .map(|(relation, state)| (relation.qualified_name(), *state))
            .collect())
    }

    pub fn sequences_snapshot(&self) -> StorageBackendResult<BTreeMap<String, SequenceState>> {
        self.try_sequences_snapshot()
    }

    /// Resolve a sequence name through the current `search_path` and return
    /// its canonical name with the current state.
    pub fn sequence_state(
        &self,
        name: &str,
    ) -> StorageBackendResult<Option<(String, SequenceState)>> {
        let Some(canonical) = self.try_resolve_sequence_name(name)? else {
            return Ok(None);
        };
        let relation = Self::resolved_relation_identity(&canonical)?;
        let seqs = self.durable.sequences.read();
        Ok(seqs.get(&relation).copied().map(|state| (canonical, state)))
    }
}
