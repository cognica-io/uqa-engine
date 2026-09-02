//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{
    BTreeMap, Engine, RelationIdentity, SQLError, SequenceBound, SequenceDataType,
    SequenceOwnerDependency, SequenceRestart, SequenceState, StorageBackendError,
    StorageBackendResult,
};
use crate::engine_capabilities::RelationResolution;
use crate::engine_state::SequenceSecurity;
use uqa_sql::ast::RelationPersistence;

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
                    Self::default_sequence_state(start, increment, SequenceDataType::BigInt),
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
            let (type_min, type_max) = sequence.data_type.bounds();
            engine.create_sequence_inner(
                &sequence.name,
                SequenceState {
                    start: sequence.start,
                    increment: sequence.increment,
                    current: sequence.start,
                    called: false,
                    log_count: 0,
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
                    owner: None,
                },
                sequence.if_not_exists,
                sequence.persistence,
                &sequence.ownership,
            )
        })
    }

    pub(crate) fn create_sequence_with_persistence(
        &self,
        name: &str,
        start: i64,
        increment: i64,
        data_type: SequenceDataType,
        if_not_exists: bool,
        persistence: uqa_sql::ast::RelationPersistence,
    ) -> Result<bool, String> {
        self.with_implicit_string_transaction(|engine| {
            engine
                .create_sequence_inner(
                    name,
                    Self::default_sequence_state(start, increment, data_type),
                    if_not_exists,
                    persistence,
                    &uqa_sql::ast::SequenceOwnership::Unchanged,
                )
                .map_err(|error| error.to_string())
        })
    }

    fn default_sequence_state(
        start: i64,
        increment: i64,
        data_type: SequenceDataType,
    ) -> SequenceState {
        let (type_min, type_max) = data_type.bounds();
        SequenceState {
            start,
            increment,
            current: start,
            called: false,
            log_count: 0,
            data_type,
            min_value: if increment > 0 { 1 } else { type_min },
            max_value: if increment > 0 { type_max } else { -1 },
            cycle: false,
            cache_size: 1,
            definition_generation: [0; 16],
            owner: None,
        }
    }

    fn create_sequence_inner(
        &self,
        name: &str,
        mut state: SequenceState,
        if_not_exists: bool,
        persistence: uqa_sql::ast::RelationPersistence,
        ownership: &uqa_sql::ast::SequenceOwnership,
    ) -> Result<bool, SQLError> {
        Self::validate_sequence_definition(state, false)?;
        let name = if persistence == uqa_sql::ast::RelationPersistence::Temporary {
            self.try_temporary_relation_name_for_create(name)?
        } else {
            self.try_relation_name_for_sql_create(name)?
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
        state.owner = self.resolve_sequence_ownership(&name, ownership)?;
        let role_owner = self.current_user_name();
        let security = SequenceSecurity {
            role_owner,
            acl: None,
        };
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
                    &Self::sequence_row(&name, object_id, state, persistence, &security).map_err(
                        |error| SQLError::Internal(format!("build sequence catalog row: {error}")),
                    )?,
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
            .insert(relation.clone(), persistence);
        self.durable
            .sequence_security
            .write()
            .insert(relation, security);
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
        let Some(name) = self.alter_sequence_target_name(alter)? else {
            return Ok(false);
        };
        let relation = Self::resolved_relation_identity(&name)
            .map_err(|error| SQLError::Internal(format!("resolve sequence `{name}`: {error}")))?;
        if let Some(role_owner) = alter.role_owner.as_deref() {
            Self::validate_sequence_role_owner_shape(alter)?;
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
        let target_persistence =
            Self::altered_sequence_persistence(alter, persistence, &relation.name)?;
        if target_persistence == persistence && Self::sequence_alter_is_persistence_only(alter) {
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
        let mut state = Self::altered_sequence_state(state, alter)?;
        if alter.ownership != uqa_sql::ast::SequenceOwnership::Unchanged {
            let owner = self.resolve_sequence_ownership(&name, &alter.ownership)?;
            if state
                .owner
                .is_some_and(|current| current.dependency == SequenceOwnerDependency::Internal)
            {
                let owner_table = self
                    .sequence_owner_target(state.owner.expect("identity owner was checked"))
                    .map_or_else(|| "<missing>".into(), |(table, _)| table);
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

    fn altered_sequence_persistence(
        alter: &uqa_sql::ast::AlterSequence,
        current: RelationPersistence,
        relation_name: &str,
    ) -> Result<RelationPersistence, SQLError> {
        match alter.persistence {
            None => Ok(current),
            Some(RelationPersistence::Permanent | RelationPersistence::Unlogged)
                if current == RelationPersistence::Temporary =>
            {
                Err(SQLError::Routine {
                    sqlstate: "42P16".into(),
                    message: format!(
                        "cannot change logged status of table \"{relation_name}\" because it is temporary"
                    ),
                })
            }
            Some(requested @ (RelationPersistence::Permanent | RelationPersistence::Unlogged)) => {
                Ok(requested)
            }
            Some(RelationPersistence::Temporary) => Err(SQLError::Internal(
                "ALTER SEQUENCE cannot request temporary persistence".into(),
            )),
        }
    }

    fn sequence_alter_is_persistence_only(alter: &uqa_sql::ast::AlterSequence) -> bool {
        alter.persistence.is_some()
            && alter.role_owner.is_none()
            && alter.restart == SequenceRestart::Unchanged
            && alter.increment.is_none()
            && alter.start.is_none()
            && alter.data_type.is_none()
            && alter.min_value == SequenceBound::Unchanged
            && alter.max_value == SequenceBound::Unchanged
            && alter.cycle.is_none()
            && alter.cache_size.is_none()
            && alter.ownership == uqa_sql::ast::SequenceOwnership::Unchanged
    }

    fn validate_sequence_role_owner_shape(
        alter: &uqa_sql::ast::AlterSequence,
    ) -> Result<(), SQLError> {
        if alter.restart != SequenceRestart::Unchanged
            || alter.increment.is_some()
            || alter.start.is_some()
            || alter.data_type.is_some()
            || alter.min_value != SequenceBound::Unchanged
            || alter.max_value != SequenceBound::Unchanged
            || alter.cycle.is_some()
            || alter.cache_size.is_some()
            || alter.ownership != uqa_sql::ast::SequenceOwnership::Unchanged
            || alter.persistence.is_some()
            || alter.lifecycle != uqa_sql::ast::SequenceLifecycle::Unchanged
        {
            return Err(SQLError::Internal(
                "ALTER SEQUENCE OWNER TO cannot contain another action".into(),
            ));
        }
        Ok(())
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

    fn altered_sequence_state(
        mut state: SequenceState,
        alter: &uqa_sql::ast::AlterSequence,
    ) -> Result<SequenceState, SQLError> {
        let resets_log_count = alter.data_type.is_some()
            || alter.increment.is_some()
            || alter.min_value != SequenceBound::Unchanged
            || alter.max_value != SequenceBound::Unchanged
            || alter.cycle.is_some()
            || alter.cache_size.is_some()
            || alter.restart != SequenceRestart::Unchanged;
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
        if resets_log_count {
            state.log_count = 0;
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

    pub(crate) fn validate_sequence_definition(
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
        self.drop_sequences_sql_inner_with_owner(names, cascade, false)
    }

    fn drop_sequences_sql_inner_with_owner(
        &self,
        names: &[String],
        cascade: bool,
        owner_initiated: bool,
    ) -> Result<(), SQLError> {
        let mut cascade_columns = Vec::new();
        let mut direct_views = Vec::new();
        for name in names {
            if !owner_initiated {
                let relation = Self::resolved_relation_identity(name).map_err(|error| {
                    SQLError::Internal(format!("resolve sequence `{name}`: {error}"))
                })?;
                self.ensure_sequence_owner(name, &relation)?;
                let owner = self
                    .durable
                    .sequences
                    .read()
                    .get(&relation)
                    .and_then(|state| state.owner)
                    .filter(|owner| owner.dependency == SequenceOwnerDependency::Internal);
                if let Some(owner) = owner {
                    let (table, column) = self.sequence_owner_target(owner).ok_or_else(|| {
                        SQLError::Internal(format!(
                            "identity sequence `{name}` has a dangling owner dependency"
                        ))
                    })?;
                    return Err(SQLError::Routine {
                        sqlstate: "2BP01".into(),
                        message: format!(
                            "cannot drop sequence {name} because column {column} of table {table} requires it"
                        ),
                    });
                }
            }
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
            self.drop_views_inner(&cascade_views, false)?;
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
            self.durable.sequence_security.write().remove(&relation);
            let mut session = self.session.state.write();
            session
                .sequence_currvals
                .retain(|_, current| current.object_id != object_id);
            if session
                .last_sequence
                .as_ref()
                .is_some_and(|last| last.object_id == object_id)
            {
                session.last_sequence = None;
            }
            drop(session);
            self.session
                .sequence_caches
                .lock()
                .retain(|_, cache| cache.object_id != object_id);
            self.note_catalog_registry_changed();
        }
        Ok(removed)
    }

    pub(crate) fn drop_owned_sequence(
        &self,
        name: &str,
        cascade: bool,
    ) -> StorageBackendResult<()> {
        let canonical = self.try_resolve_sequence_name(name)?.ok_or_else(|| {
            StorageBackendError::Other(format!("owned sequence `{name}` does not exist"))
        })?;
        self.drop_sequences_sql_inner_with_owner(std::slice::from_ref(&canonical), cascade, true)
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
