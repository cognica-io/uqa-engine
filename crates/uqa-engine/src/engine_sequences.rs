//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{
    BTreeMap, CatalogFacade, Engine, RelationIdentity, SQLError, SequenceBound, SequenceDataType,
    SequenceOptions, SequenceRestart, SequenceRow, SequenceState, StorageBackendError,
    StorageBackendResult, SEQUENCES_METADATA_KEY,
};

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

    /// Resolve a reference read from legacy persisted metadata without using
    /// the current session's `search_path`. An unqualified local name is safe
    /// only when exactly one catalog sequence has that name.
    pub(crate) fn resolve_stored_sequence_reference(
        &self,
        reference: &str,
    ) -> StorageBackendResult<String> {
        self.refresh_sequences_from_catalog()?;
        self.resolve_stored_sequence_reference_from_loaded_registry(reference)
    }

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
            engine
                .create_sequence_inner(
                    name,
                    Self::default_sequence_state(start, increment),
                    if_not_exists,
                    uqa_sql::ast::RelationPersistence::Permanent,
                )
                .map_err(|error| error.to_string())
        })
    }

    pub(crate) fn create_sequence_sql(
        &self,
        sequence: &uqa_sql::ast::CreateSequence,
    ) -> Result<bool, SQLError> {
        self.with_implicit_transaction(|engine| {
            let (type_min, type_max) = sequence.data_type.bounds();
            engine.create_sequence_inner(
                &sequence.name,
                SequenceState {
                    start: sequence.start,
                    increment: sequence.increment,
                    current: sequence.start,
                    called: false,
                    data_type: sequence.data_type,
                    min_value: sequence.min_value.unwrap_or(if sequence.increment > 0 {
                        1
                    } else {
                        type_min
                    }),
                    max_value: sequence.max_value.unwrap_or(if sequence.increment > 0 {
                        type_max
                    } else {
                        -1
                    }),
                    cycle: sequence.cycle,
                    cache_size: sequence.cache_size,
                    definition_generation: [0; 16],
                },
                sequence.if_not_exists,
                sequence.persistence,
            )
        })
    }

    pub(crate) fn create_sequence_with_persistence(
        &self,
        name: &str,
        start: i64,
        increment: i64,
        if_not_exists: bool,
        persistence: uqa_sql::ast::RelationPersistence,
    ) -> Result<bool, String> {
        self.with_implicit_string_transaction(|engine| {
            engine
                .create_sequence_inner(
                    name,
                    Self::default_sequence_state(start, increment),
                    if_not_exists,
                    persistence,
                )
                .map_err(|error| error.to_string())
        })
    }

    fn default_sequence_state(start: i64, increment: i64) -> SequenceState {
        SequenceState {
            start,
            increment,
            current: start,
            called: false,
            data_type: SequenceDataType::BigInt,
            min_value: if increment > 0 { 1 } else { i64::MIN },
            max_value: if increment > 0 { i64::MAX } else { -1 },
            cycle: false,
            cache_size: 1,
            definition_generation: [0; 16],
        }
    }

    fn create_sequence_inner(
        &self,
        name: &str,
        mut state: SequenceState,
        if_not_exists: bool,
        persistence: uqa_sql::ast::RelationPersistence,
    ) -> Result<bool, SQLError> {
        Self::validate_sequence_definition(state, false)?;
        let name = if persistence == uqa_sql::ast::RelationPersistence::Temporary {
            self.try_temporary_relation_name_for_create(name)
                .map_err(SQLError::Unsupported)?
        } else {
            self.try_relation_name_for_create(name)
                .map_err(SQLError::Unsupported)?
        };
        let relation = Self::resolved_relation_identity(&name)
            .map_err(|error| SQLError::Internal(format!("resolve sequence `{name}`: {error}")))?;
        self.refresh_sequences_from_catalog().map_err(|error| {
            SQLError::Internal(format!("load sequence catalog for `{name}`: {error}"))
        })?;
        if self
            .relation_kind_at(&name)
            .map_err(|error| SQLError::Internal(format!("resolve relation `{name}`: {error}")))?
            .is_some()
        {
            return Self::sequence_create_collision(&name, if_not_exists);
        }
        let object_id = crate::new_sequence_object_id().map_err(|error| {
            SQLError::Internal(format!("allocate sequence `{name}` identity: {error}"))
        })?;
        state.definition_generation = object_id;
        if persistence == uqa_sql::ast::RelationPersistence::Temporary {
            let seqs = self.durable.sequences.read();
            if seqs.contains_key(&relation) {
                return Self::sequence_create_collision(&name, if_not_exists);
            }
        } else if let Some(catalog) = self.storage.catalog.as_ref() {
            let created = catalog
                .create_sequence_row(
                    &Self::sequence_row(&name, object_id, state, persistence).map_err(|error| {
                        SQLError::Internal(format!("build sequence catalog row: {error}"))
                    })?,
                )
                .map_err(|error| {
                    SQLError::Internal(format!("persist sequence catalog: {error}"))
                })?;
            if !created {
                return Self::sequence_create_collision(&name, if_not_exists);
            }
        } else {
            let seqs = self.durable.sequences.read();
            if seqs.contains_key(&relation) {
                return Self::sequence_create_collision(&name, if_not_exists);
            }
        }
        self.durable
            .sequences
            .write()
            .insert(relation.clone(), state);
        self.durable
            .sequence_object_ids
            .write()
            .insert(relation.clone(), object_id);
        self.durable
            .sequence_persistence
            .write()
            .insert(relation, persistence);
        self.note_catalog_registry_changed();
        Ok(true)
    }

    fn sequence_create_collision(name: &str, if_not_exists: bool) -> Result<bool, SQLError> {
        if if_not_exists {
            Ok(false)
        } else {
            Err(SQLError::Routine {
                sqlstate: "42P07".into(),
                message: format!("relation \"{name}\" already exists"),
            })
        }
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
        let name = match self
            .try_resolve_relation_kind(&alter.name)
            .map_err(|error| {
                SQLError::Internal(format!(
                    "load sequence catalog for `{}`: {error}",
                    alter.name
                ))
            })? {
            Some((name, "sequence")) => name,
            Some((_name, _kind)) => {
                return Err(SQLError::Routine {
                    sqlstate: "42809".into(),
                    message: format!("\"{}\" is not a sequence", alter.name),
                })
            }
            None if alter.if_exists => return Ok(false),
            None => {
                return Err(SQLError::Routine {
                    sqlstate: "42P01".into(),
                    message: format!("relation \"{}\" does not exist", alter.name),
                })
            }
        };
        let relation = Self::resolved_relation_identity(&name)
            .map_err(|error| SQLError::Internal(format!("resolve sequence `{name}`: {error}")))?;
        let persistence = self
            .durable
            .sequence_persistence
            .read()
            .get(&relation)
            .copied()
            .unwrap_or_default();
        let object_id = self
            .durable
            .sequence_object_ids
            .read()
            .get(&relation)
            .copied()
            .ok_or_else(|| {
                SQLError::Internal(format!("sequence `{name}` has no object identity"))
            })?;
        let temporary = persistence == uqa_sql::ast::RelationPersistence::Temporary;
        let state = self
            .durable
            .sequences
            .read()
            .get(&relation)
            .copied()
            .ok_or_else(|| SQLError::Internal(format!("sequence `{name}` disappeared")))?;
        let mut state = Self::altered_sequence_state(state, alter)?;
        let definition_generation =
            crate::new_sequence_definition_generation().map_err(|error| {
                SQLError::Internal(format!(
                    "allocate sequence `{name}` definition generation: {error}"
                ))
            })?;
        state.definition_generation = definition_generation;
        if !temporary {
            if let Some(catalog) = self.storage.catalog.as_ref() {
                if !catalog
                    .replace_sequence_row(
                        &Self::sequence_row(&name, object_id, state, persistence).map_err(
                            |error| {
                                SQLError::Internal(format!("build sequence catalog row: {error}"))
                            },
                        )?,
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
        self.session.sequence_caches.lock().remove(&relation);
        self.note_catalog_registry_changed();
        Ok(true)
    }

    fn altered_sequence_state(
        mut state: SequenceState,
        alter: &uqa_sql::ast::AlterSequence,
    ) -> Result<SequenceState, SQLError> {
        if let Some(data_type) = alter.data_type {
            let (old_type_min, old_type_max) = state.data_type.bounds();
            let (new_type_min, new_type_max) = data_type.bounds();
            if state.min_value == old_type_min {
                state.min_value = new_type_min;
            }
            if state.max_value == old_type_max {
                state.max_value = new_type_max;
            }
            state.data_type = data_type;
        }
        if let Some(increment) = alter.increment {
            state.increment = increment;
        }
        let (type_min, type_max) = state.data_type.bounds();
        match alter.min_value {
            SequenceBound::Unchanged => {}
            SequenceBound::Default => {
                state.min_value = if state.increment > 0 { 1 } else { type_min };
            }
            SequenceBound::Value(value) => state.min_value = value,
        }
        match alter.max_value {
            SequenceBound::Unchanged => {}
            SequenceBound::Default => {
                state.max_value = if state.increment > 0 { type_max } else { -1 };
            }
            SequenceBound::Value(value) => state.max_value = value,
        }
        if let Some(start_val) = alter.start {
            state.start = start_val;
        }
        if let Some(cycle) = alter.cycle {
            state.cycle = cycle;
        }
        if let Some(cache_size) = alter.cache_size {
            state.cache_size = cache_size;
        }
        if alter.restart != SequenceRestart::Unchanged {
            let restart_val = match alter.restart {
                SequenceRestart::Unchanged => unreachable!("restart action was checked above"),
                SequenceRestart::FromStart => state.start,
                SequenceRestart::With(value) => value,
            };
            state.current = restart_val;
            state.called = false;
        }
        Self::validate_sequence_definition(state, true)?;
        Ok(state)
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

    fn validate_sequence_definition(
        state: SequenceState,
        validate_current: bool,
    ) -> Result<(), SQLError> {
        let invalid = |message| SQLError::Routine {
            sqlstate: "22023".into(),
            message,
        };
        if state.increment == 0 {
            return Err(invalid("INCREMENT must not be zero".into()));
        }
        if state.cache_size <= 0 {
            return Err(invalid(format!(
                "CACHE ({}) must be greater than zero",
                state.cache_size
            )));
        }
        let (type_min, type_max) = state.data_type.bounds();
        if !(type_min..=type_max).contains(&state.max_value) {
            return Err(invalid(format!(
                "MAXVALUE ({}) is out of range for sequence data type {}",
                state.max_value,
                state.data_type.sql_name()
            )));
        }
        if !(type_min..=type_max).contains(&state.min_value) {
            return Err(invalid(format!(
                "MINVALUE ({}) is out of range for sequence data type {}",
                state.min_value,
                state.data_type.sql_name()
            )));
        }
        if state.min_value >= state.max_value {
            return Err(invalid(format!(
                "MINVALUE ({}) must be less than MAXVALUE ({})",
                state.min_value, state.max_value
            )));
        }
        if state.start < state.min_value {
            return Err(invalid(format!(
                "START value ({}) cannot be less than MINVALUE ({})",
                state.start, state.min_value
            )));
        }
        if state.start > state.max_value {
            return Err(invalid(format!(
                "START value ({}) cannot be greater than MAXVALUE ({})",
                state.start, state.max_value
            )));
        }
        if validate_current && state.current < state.min_value {
            return Err(invalid(format!(
                "RESTART value ({}) cannot be less than MINVALUE ({})",
                state.current, state.min_value
            )));
        }
        if validate_current && state.current > state.max_value {
            return Err(invalid(format!(
                "RESTART value ({}) cannot be greater than MAXVALUE ({})",
                state.current, state.max_value
            )));
        }
        Ok(())
    }

    pub fn drop_sequence(&self, name: &str) -> Result<bool, String> {
        self.with_implicit_string_transaction(|engine| engine.drop_sequence_inner(name))
    }

    fn drop_sequence_inner(&self, name: &str) -> Result<bool, String> {
        let Some(name) = self
            .try_resolve_sequence_name(name)
            .map_err(|err| format!("load sequence catalog: {err}"))?
        else {
            return Ok(false);
        };
        self.drop_sequences_sql_inner(std::slice::from_ref(&name), false)
            .map_err(|error| error.to_string())?;
        Ok(true)
    }

    pub(crate) fn drop_sequences_sql_inner(
        &self,
        names: &[String],
        cascade: bool,
    ) -> Result<(), SQLError> {
        let mut cascade_columns = Vec::new();
        let mut direct_views = Vec::new();
        for name in names {
            let columns = self
                .sequence_schema_expression_dependents(name)
                .map_err(|error| {
                    SQLError::Internal(format!(
                        "inspect column dependencies for sequence `{name}`: {error}"
                    ))
                })?;
            let views = self.views_depending_on_sequence(name).map_err(|error| {
                SQLError::Internal(format!(
                    "inspect view dependencies for sequence `{name}`: {error}"
                ))
            })?;
            if !cascade && (!columns.is_empty() || !views.is_empty()) {
                let mut dependents = columns
                    .iter()
                    .map(|(table, column)| format!("{table}.{column}"))
                    .collect::<Vec<_>>();
                dependents.extend(views.iter().map(|view| format!("view {view}")));
                return Err(SQLError::Routine {
                    sqlstate: "2BP01".into(),
                    message: format!(
                        "cannot drop sequence {name} because other objects depend on it: {}",
                        dependents.join(", ")
                    ),
                });
            }
            cascade_columns.extend(columns);
            direct_views.extend(views);
        }
        cascade_columns.sort();
        cascade_columns.dedup();
        let cascade_views = self.cascade_view_closure(direct_views)?;
        if cascade && !cascade_views.is_empty() {
            self.drop_views_inner(&cascade_views)?;
        }
        for name in names {
            self.detach_sequence_column_dependencies(name, cascade)
                .map_err(|error| {
                    SQLError::Internal(format!(
                        "detach column dependencies for sequence `{name}`: {error}"
                    ))
                })?;
            if !self
                .remove_sequence_state_inner(name)
                .map_err(SQLError::Internal)?
            {
                return Err(SQLError::Internal(format!(
                    "resolved sequence `{name}` disappeared before DROP"
                )));
            }
        }
        if cascade {
            let mut dependents = cascade_columns
                .iter()
                .map(|(table, column)| {
                    format!("default value for column {column} of table {table}")
                })
                .collect::<Vec<_>>();
            dependents.extend(cascade_views.iter().map(|view| format!("view {view}")));
            match dependents.as_slice() {
                [] => {}
                [dependent] => {
                    self.push_sql_notice("NOTICE", &format!("drop cascades to {dependent}"));
                }
                _ => self.push_sql_notice(
                    "NOTICE",
                    &format!("drop cascades to {} other objects", dependents.len()),
                ),
            }
        }
        Ok(())
    }

    fn remove_sequence_state_inner(&self, name: &str) -> Result<bool, String> {
        let relation = Self::resolved_relation_identity(name)
            .map_err(|err| format!("resolve sequence `{name}`: {err}"))?;
        let object_id = self
            .durable
            .sequence_object_ids
            .read()
            .get(&relation)
            .copied()
            .ok_or_else(|| format!("Sequence `{name}` has no object identity"))?;
        let temporary = self
            .durable
            .sequence_persistence
            .read()
            .get(&relation)
            .is_some_and(|persistence| {
                *persistence == uqa_sql::ast::RelationPersistence::Temporary
            });
        let removed = if temporary {
            self.durable.sequences.read().contains_key(&relation)
        } else if let Some(catalog) = self.storage.catalog.as_ref() {
            catalog
                .drop_sequence_row(name)
                .map_err(|err| format!("persist sequence catalog: {err}"))?
        } else {
            self.durable.sequences.read().contains_key(&relation)
        };
        if removed {
            self.durable.sequences.write().remove(&relation);
            self.durable.sequence_object_ids.write().remove(&relation);
            self.durable.sequence_persistence.write().remove(&relation);
            let mut session = self.session.state.write();
            session.sequence_currvals.remove(&relation);
            if session
                .last_sequence
                .as_ref()
                .is_some_and(|last| last.relation == relation && last.object_id == object_id)
            {
                session.last_sequence = None;
            }
            drop(session);
            self.session.sequence_caches.lock().remove(&relation);
            self.note_catalog_registry_changed();
        }
        Ok(removed)
    }

    pub(crate) fn drop_owned_sequence(&self, name: &str) -> StorageBackendResult<()> {
        self.drop_sequence_inner(name)
            .and_then(|removed| {
                if removed {
                    Ok(())
                } else {
                    Err(format!("owned sequence `{name}` does not exist"))
                }
            })
            .map_err(StorageBackendError::Other)
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

    fn sequence_row(
        name: &str,
        object_id: [u8; 16],
        state: SequenceState,
        persistence: uqa_sql::ast::RelationPersistence,
    ) -> StorageBackendResult<SequenceRow> {
        Ok(SequenceRow {
            relation: RelationIdentity::from_legacy_name(name)
                .map_err(StorageBackendError::Other)?,
            object_id,
            definition_generation: state.definition_generation,
            start: state.start,
            increment: state.increment,
            current: state.current,
            called: state.called,
            persistence: persistence.catalog_code().into(),
            options: SequenceOptions {
                data_type: state.data_type.sql_name().into(),
                min_value: Some(state.min_value),
                max_value: Some(state.max_value),
                cycle: state.cycle,
                cache_size: state.cache_size,
            },
        })
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

    /// Consume the legacy all-sequences metadata snapshot during initial
    /// engine open.  Runtime catalog reloads must never call this migration:
    /// they can run inside a pinned read transaction or after a physical
    /// rollback, where clearing metadata would silently turn restoration into
    /// a database write.
    pub(crate) fn migrate_legacy_sequences_from_metadata(
        catalog: &dyn CatalogFacade,
    ) -> StorageBackendResult<()> {
        // One-time, restart-safe migration from the former all-sequences JSON
        // snapshot. Merge idempotently even after a partially completed run,
        // then clear the legacy payload so deliberately dropping every typed
        // sequence cannot resurrect the old snapshot on the next open.
        if let Some(json) = catalog.get_metadata(SEQUENCES_METADATA_KEY)? {
            let legacy = serde_json::from_str::<BTreeMap<String, SequenceState>>(&json)?;
            if !legacy.is_empty() {
                for (name, state) in legacy {
                    catalog.create_sequence_row(&Self::sequence_row(
                        &name,
                        crate::new_sequence_object_id()?,
                        state,
                        uqa_sql::ast::RelationPersistence::Permanent,
                    )?)?;
                }
                catalog.set_metadata(SEQUENCES_METADATA_KEY, "{}")?;
            }
        }
        Ok(())
    }

    pub(crate) fn migrate_sequence_identities(
        catalog: &dyn CatalogFacade,
    ) -> StorageBackendResult<()> {
        let mut identities = std::collections::BTreeSet::new();
        for mut row in catalog.load_sequence_rows()? {
            let mut changed = false;
            if row.object_id == [0; 16] || !identities.insert(row.object_id) {
                loop {
                    let object_id = crate::new_sequence_object_id()?;
                    if identities.insert(object_id) {
                        row.object_id = object_id;
                        changed = true;
                        break;
                    }
                }
            }
            if row.definition_generation == [0; 16] {
                row.definition_generation = row.object_id;
                changed = true;
            }
            if !changed {
                continue;
            }
            if !catalog.replace_sequence_row(&row)? {
                return Err(StorageBackendError::Other(format!(
                    "sequence `{}` disappeared while assigning its object identity",
                    row.relation.qualified_name()
                )));
            }
        }
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
        let temporary_persistence = self
            .durable
            .sequence_persistence
            .read()
            .iter()
            .filter(|(_, persistence)| {
                **persistence == uqa_sql::ast::RelationPersistence::Temporary
            })
            .map(|(relation, persistence)| (relation.clone(), *persistence))
            .collect::<BTreeMap<_, _>>();
        let mut sequences = self
            .durable
            .sequences
            .read()
            .iter()
            .filter(|(relation, _)| temporary_persistence.contains_key(*relation))
            .map(|(relation, state)| (relation.clone(), *state))
            .collect::<BTreeMap<_, _>>();
        let mut object_ids = self
            .durable
            .sequence_object_ids
            .read()
            .iter()
            .filter(|(relation, _)| temporary_persistence.contains_key(*relation))
            .map(|(relation, object_id)| (relation.clone(), *object_id))
            .collect::<BTreeMap<_, _>>();
        let mut seen_object_ids = object_ids
            .values()
            .copied()
            .collect::<std::collections::BTreeSet<_>>();
        let mut persistence = temporary_persistence;
        for row in rows {
            let name = row.relation.qualified_name();
            if row.object_id == [0; 16] {
                return Err(StorageBackendError::Other(format!(
                    "corrupt sequence `{name}` has no object identity"
                )));
            }
            if !seen_object_ids.insert(row.object_id) {
                return Err(StorageBackendError::Other(format!(
                    "corrupt sequence `{name}` has a duplicate object identity"
                )));
            }
            let object_id = row.object_id;
            let stored = match row.persistence.as_str() {
                "p" => uqa_sql::ast::RelationPersistence::Permanent,
                "u" => uqa_sql::ast::RelationPersistence::Unlogged,
                other => {
                    return Err(StorageBackendError::Other(format!(
                        "corrupt sequence `{name}` persistence `{other}`"
                    )))
                }
            };
            let (relation, state) = Self::sequence_state_from_row(row)?;
            persistence.insert(relation.clone(), stored);
            object_ids.insert(relation.clone(), object_id);
            sequences.insert(relation, state);
        }
        *self.durable.sequences.write() = sequences;
        *self.durable.sequence_object_ids.write() = object_ids;
        *self.durable.sequence_persistence.write() = persistence;
        Ok(())
    }

    fn sequence_state_from_row(
        row: SequenceRow,
    ) -> StorageBackendResult<(RelationIdentity, SequenceState)> {
        if row.increment == 0 {
            return Err(StorageBackendError::Other(format!(
                "corrupt sequence `{}` has zero increment",
                row.relation.qualified_name()
            )));
        }
        let data_type = match row.options.data_type.as_str() {
            "smallint" => SequenceDataType::SmallInt,
            "integer" => SequenceDataType::Integer,
            "bigint" => SequenceDataType::BigInt,
            other => {
                return Err(StorageBackendError::Other(format!(
                    "corrupt sequence `{}` has data type `{other}`",
                    row.relation.qualified_name()
                )))
            }
        };
        let (type_min, type_max) = data_type.bounds();
        let state = SequenceState {
            start: row.start,
            increment: row.increment,
            current: row.current,
            called: row.called,
            data_type,
            min_value: row.options.min_value.unwrap_or(if row.increment > 0 {
                1
            } else {
                type_min
            }),
            max_value: row.options.max_value.unwrap_or(if row.increment > 0 {
                type_max
            } else {
                -1
            }),
            cycle: row.options.cycle,
            cache_size: row.options.cache_size,
            definition_generation: row.definition_generation,
        };
        if state.definition_generation == [0; 16] {
            return Err(StorageBackendError::Other(format!(
                "corrupt sequence `{}` has no definition generation",
                row.relation.qualified_name()
            )));
        }
        Self::validate_sequence_definition(state, false).map_err(|error| {
            StorageBackendError::Other(format!(
                "corrupt sequence `{}` definition: {error}",
                row.relation.qualified_name()
            ))
        })?;
        Ok((row.relation, state))
    }

    // -----------------------------------------------------------------
    // Prepared statements.
    // -----------------------------------------------------------------

    pub fn register_prepared(
        &self,
        name: String,
        definition: uqa_sql::ast::Statement,
    ) -> Result<(), uqa_sql::SQLError> {
        let plan = uqa_planner::UnifiedPlan::lower_with(definition, &|aggregate: &str| {
            self.has_registered_aggregate_function(aggregate)
        });
        self.register_prepared_plan(name, plan)
    }

    pub(crate) fn register_prepared_plan(
        &self,
        name: String,
        logical_plan: uqa_planner::UnifiedPlan,
    ) -> Result<(), uqa_sql::SQLError> {
        let plan = crate::sql::optimize_engine_plan(self, logical_plan.clone())?;
        self.session
            .state
            .write()
            .prepared
            .insert(name, super::PreparedStatementPlan { logical_plan, plan });
        Ok(())
    }

    pub fn lookup_prepared(&self, name: &str) -> Option<uqa_planner::UnifiedPlan> {
        self.session
            .state
            .read()
            .prepared
            .get(name)
            .map(|entry| entry.plan.clone())
    }

    pub(crate) fn rebind_prepared_plans(&self) -> Result<(), uqa_sql::SQLError> {
        let plans = self
            .session
            .state
            .read()
            .prepared
            .iter()
            .map(|(name, prepared)| (name.clone(), prepared.logical_plan.clone()))
            .collect::<Vec<_>>();
        let mut rebound = Vec::with_capacity(plans.len());
        for (name, plan) in plans {
            rebound.push((name, crate::sql::optimize_engine_plan(self, plan)?));
        }
        let mut session = self.session.state.write();
        for (name, plan) in rebound {
            if let Some(entry) = session.prepared.get_mut(&name) {
                entry.plan = plan;
            }
        }
        Ok(())
    }

    pub fn deallocate_prepared(&self, name: Option<&str>) {
        match name {
            Some(n) => {
                self.session.state.write().prepared.remove(n);
            }
            None => self.session.state.write().prepared.clear(),
        }
    }
}
