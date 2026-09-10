//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{
    BTreeMap, Engine, RelationIdentity, SQLError, SequenceDataType, SequenceOwnerDependency,
    SequenceRestart, SequenceState, StorageBackendError, StorageBackendResult,
};
use crate::capabilities::RelationResolution;

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
            crate::sql::sequence_relation_oid(object_id).to_string(),
        ))
    }

    /// Resolve a reference read from persisted metadata against the loaded registry without using the current session's `search_path`. An unqualified local name is safe only when exactly one catalog sequence has that name.
    pub(crate) fn resolve_stored_sequence_reference_from_loaded_registry(
        &self,
        reference: &str,
    ) -> StorageBackendResult<String> {
        let (schema, local_name) =
            RelationIdentity::parse_reference(reference).map_err(|error| {
                StorageBackendError::Other(format!(
                    "invalid persisted sequence reference `{reference}`: {error}"
                ))
            })?;
        let sequences = self.durable.sequences.read();
        if let Some(schema) = schema {
            let target = RelationIdentity::new(schema, local_name);
            if sequences.contains_key(&target) {
                return Ok(target.qualified_name());
            }
            return Err(StorageBackendError::Other(format!(
                "dangling persisted sequence reference `{reference}`"
            )));
        }

        let candidates = sequences
            .keys()
            .filter(|candidate| candidate.name == local_name)
            .map(RelationIdentity::qualified_name)
            .collect::<Vec<_>>();
        match candidates.as_slice() {
            [target] => Ok(target.clone()),
            [] => Err(StorageBackendError::Other(format!(
                "dangling persisted sequence reference `{reference}`"
            ))),
            _ => Err(StorageBackendError::Other(format!(
                "ambiguous persisted sequence reference `{reference}` matches {}",
                candidates.join(", ")
            ))),
        }
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

    pub(crate) fn create_sequence_sql(
        &self,
        sequence: &uqa_sql::ast::CreateSequence,
    ) -> Result<bool, SQLError> {
        self.with_implicit_transaction(|engine| {
            uqa_execution::schema::sequences::creation::create_sequence(
                &engine.sequence_creation_context(),
                &sequence.name,
                SequenceState::from_definition(
                    uqa_sql::schema::sequences::definition::SequenceDefinition::from_create(
                        sequence,
                    ),
                ),
                sequence.if_not_exists,
                sequence.persistence,
                &sequence.ownership,
            )
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

    pub(crate) fn alter_sequence_sql(
        &self,
        alter: &uqa_sql::ast::AlterSequence,
    ) -> Result<bool, SQLError> {
        self.with_implicit_transaction(|engine| engine.alter_sequence_inner(alter))
    }

    fn alter_sequence_inner(&self, alter: &uqa_sql::ast::AlterSequence) -> Result<bool, SQLError> {
        let Some(name) = self.alter_sequence_target_name(alter)? else {
            return Ok(false);
        };
        let relation = Self::resolved_relation_identity(&name)
            .map_err(|error| SQLError::Internal(format!("resolve sequence `{name}`: {error}")))?;
        if let Some(role_owner) = alter.role_owner.as_deref() {
            uqa_sql::schema::sequences::actions::validate_sequence_role_owner_shape(alter)?;
            self.alter_sequence_role_owner_inner(&name, &relation, role_owner)?;
            return Ok(true);
        }
        self.ensure_sequence_owner(&name, &relation)?;
        let persistence = self
            .durable
            .sequence_persistence
            .read()
            .get(&relation)
            .copied()
            .unwrap_or_default();
        if alter.lifecycle != uqa_sql::ast::SequenceLifecycle::Unchanged {
            self.alter_sequence_lifecycle_inner(&name, &relation, persistence, alter)?;
            return Ok(true);
        }
        let target_persistence = uqa_sql::schema::sequences::actions::altered_sequence_persistence(
            alter,
            persistence,
            &relation.name,
        )?;
        if target_persistence == persistence
            && uqa_sql::schema::sequences::actions::sequence_alter_is_persistence_only(alter)
        {
            return Ok(true);
        }
        let object_id = self
            .durable
            .sequence_object_ids
            .read()
            .get(&relation)
            .copied()
            .ok_or_else(|| {
                SQLError::Internal(format!("sequence `{name}` has no object identity"))
            })?;
        let state = self
            .durable
            .sequences
            .read()
            .get(&relation)
            .copied()
            .ok_or_else(|| SQLError::Internal(format!("sequence `{name}` disappeared")))?;
        let mut state = uqa_execution::catalog::sequence::altered_sequence_state(state, alter)?;
        if alter.ownership != uqa_sql::ast::SequenceOwnership::Unchanged {
            let owner = uqa_execution::schema::sequences::creation::bind_sequence_owner(
                self,
                &name,
                &alter.ownership,
            )?;
            if state
                .owner
                .is_some_and(|current| current.dependency == SequenceOwnerDependency::Internal)
            {
                let owner_table = self
                    .sequence_owner_target(state.owner.expect("identity owner was checked"))
                    .map_or_else(|| "<missing>".into(), |(table, _, _)| table);
                return Err(SQLError::Routine {
                    sqlstate: "0A000".into(),
                    message: format!(
                        "cannot change ownership of identity sequence; sequence \"{}\" is linked to table \"{owner_table}\"",
                        relation.name
                    ),
                });
            }
            state.owner = owner;
        }
        let definition_generation =
            crate::new_sequence_definition_generation().map_err(|error| {
                SQLError::Internal(format!(
                    "allocate sequence `{name}` definition generation: {error}"
                ))
            })?;
        state.definition_generation = definition_generation;
        self.persist_sequence_state_replacement(
            &name,
            &relation,
            object_id,
            target_persistence,
            state,
            alter.persistence.is_none(),
        )?;
        if alter.ownership != uqa_sql::ast::SequenceOwnership::Unchanged {
            self.clear_auto_increment_owner_markers(&name)
                .map_err(|error| {
                    SQLError::Internal(format!(
                        "detach legacy sequence owner metadata for `{name}`: {error}"
                    ))
                })?;
        }
        Ok(true)
    }

    fn alter_sequence_target_name(
        &self,
        alter: &uqa_sql::ast::AlterSequence,
    ) -> Result<Option<String>, SQLError> {
        match self.resolve_visible_relation_kind(&alter.name)? {
            RelationResolution::Found(name, "sequence") => Ok(Some(name)),
            RelationResolution::Found(_name, _kind) => Err(SQLError::Routine {
                sqlstate: "42809".into(),
                message: format!("\"{}\" is not a sequence", alter.name),
            }),
            RelationResolution::MissingRelation | RelationResolution::MissingSchema(_)
                if alter.if_exists =>
            {
                Ok(None)
            }
            RelationResolution::MissingSchema(schema) => Err(SQLError::Routine {
                sqlstate: "3F000".into(),
                message: format!("schema \"{schema}\" does not exist"),
            }),
            RelationResolution::MissingRelation => Err(SQLError::Routine {
                sqlstate: "42P01".into(),
                message: format!("relation \"{}\" does not exist", alter.name),
            }),
        }
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
