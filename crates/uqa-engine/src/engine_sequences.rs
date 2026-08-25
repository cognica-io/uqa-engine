//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{
    BTreeMap, CatalogFacade, Engine, RelationIdentity, SQLError, SequenceRestart, SequenceRow,
    SequenceState, StorageBackendError, StorageBackendResult, SEQUENCES_METADATA_KEY,
};

#[derive(Debug, thiserror::Error)]
enum SequenceValueError {
    #[error("relation \"{0}\" does not exist")]
    Undefined(String),
    #[error("currval of sequence \"{0}\" is not yet defined in this session")]
    CurrvalUndefined(String),
    #[error("nextval: reached {bound} value of sequence \"{name}\" ({value})")]
    Exhausted {
        name: String,
        bound: &'static str,
        value: i64,
    },
    #[error("{0}")]
    Internal(String),
}

impl SequenceValueError {
    fn exhausted(name: &str, state: SequenceState) -> Self {
        Self::Exhausted {
            name: name.to_string(),
            bound: if state.increment > 0 {
                "maximum"
            } else {
                "minimum"
            },
            value: state.current,
        }
    }

    fn into_sql_error(self) -> SQLError {
        let sqlstate = match self {
            Self::Undefined(_) => "42P01",
            Self::CurrvalUndefined(_) => "55000",
            Self::Exhausted { .. } => "2200H",
            Self::Internal(message) => return SQLError::Internal(message),
        };
        SQLError::Routine {
            sqlstate: sqlstate.into(),
            message: self.to_string(),
        }
    }
}

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
            engine.create_sequence_inner(
                name,
                start,
                increment,
                if_not_exists,
                uqa_sql::ast::RelationPersistence::Permanent,
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
            engine.create_sequence_inner(name, start, increment, if_not_exists, persistence)
        })
    }

    fn create_sequence_inner(
        &self,
        name: &str,
        start: i64,
        increment: i64,
        if_not_exists: bool,
        persistence: uqa_sql::ast::RelationPersistence,
    ) -> Result<bool, String> {
        Self::validate_sequence_increment(name, increment)?;
        let name = if persistence == uqa_sql::ast::RelationPersistence::Temporary {
            self.try_temporary_relation_name_for_create(name)?
        } else {
            self.try_relation_name_for_create(name)?
        };
        let relation = Self::resolved_relation_identity(&name)
            .map_err(|err| format!("resolve sequence `{name}`: {err}"))?;
        self.refresh_sequences_from_catalog()
            .map_err(|err| format!("load sequence catalog: {err}"))?;
        if let Some(kind) = self
            .relation_kind_at(&name)
            .map_err(|err| format!("resolve relation `{name}`: {err}"))?
        {
            if kind != "sequence" {
                return Err(format!("Relation `{name}` already exists as {kind}"));
            }
        }
        let state = SequenceState {
            start,
            increment,
            current: start,
            called: false,
        };
        if persistence == uqa_sql::ast::RelationPersistence::Temporary {
            let seqs = self.durable.sequences.read();
            if seqs.contains_key(&relation) {
                return if if_not_exists {
                    Ok(false)
                } else {
                    Err(format!("Sequence `{name}` already exists"))
                };
            }
        } else if let Some(catalog) = self.storage.catalog.as_ref() {
            let created = catalog
                .create_sequence_row(
                    &Self::sequence_row(&name, state, persistence)
                        .map_err(|err| format!("build sequence catalog row: {err}"))?,
                )
                .map_err(|err| format!("persist sequence catalog: {err}"))?;
            if !created {
                return if if_not_exists {
                    Ok(false)
                } else {
                    Err(format!("Sequence `{name}` already exists"))
                };
            }
        } else {
            let seqs = self.durable.sequences.read();
            if seqs.contains_key(&relation) {
                return if if_not_exists {
                    Ok(false)
                } else {
                    Err(format!("Sequence `{name}` already exists"))
                };
            }
        }
        self.durable
            .sequences
            .write()
            .insert(relation.clone(), state);
        self.durable
            .sequence_persistence
            .write()
            .insert(relation, persistence);
        self.note_catalog_registry_changed();
        Ok(true)
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
        self.with_implicit_string_transaction(|engine| {
            engine
                .alter_sequence_inner(name, restart, increment, start, false)
                .map(|_| ())
        })
    }

    pub(crate) fn alter_sequence_if_exists(
        &self,
        name: &str,
        restart: SequenceRestart,
        increment: Option<i64>,
        start: Option<i64>,
        if_exists: bool,
    ) -> Result<bool, String> {
        self.with_implicit_string_transaction(|engine| {
            engine.alter_sequence_inner(name, restart, increment, start, if_exists)
        })
    }

    fn alter_sequence_inner(
        &self,
        name: &str,
        restart: SequenceRestart,
        increment: Option<i64>,
        start: Option<i64>,
        if_exists: bool,
    ) -> Result<bool, String> {
        let Some(name) = self
            .try_resolve_sequence_name(name)
            .map_err(|err| format!("load sequence catalog: {err}"))?
        else {
            return if if_exists {
                Ok(false)
            } else {
                Err(format!("Sequence `{name}` does not exist"))
            };
        };
        if let Some(increment) = increment {
            Self::validate_sequence_increment(&name, increment)?;
        }
        let relation = Self::resolved_relation_identity(&name)
            .map_err(|err| format!("resolve sequence `{name}`: {err}"))?;
        let persistence = self
            .durable
            .sequence_persistence
            .read()
            .get(&relation)
            .copied()
            .unwrap_or_default();
        let temporary = persistence == uqa_sql::ast::RelationPersistence::Temporary;
        let mut state = self
            .durable
            .sequences
            .read()
            .get(&relation)
            .copied()
            .ok_or_else(|| format!("Sequence `{name}` does not exist"))?;
        if let Some(start_val) = start {
            state.start = start_val;
        }
        if let Some(inc) = increment {
            state.increment = inc;
        }
        if restart != SequenceRestart::Unchanged {
            let restart_val = match restart {
                SequenceRestart::Unchanged => unreachable!("restart action was checked above"),
                SequenceRestart::FromStart => state.start,
                SequenceRestart::With(value) => value,
            };
            state.current = restart_val;
            state.called = false;
        }
        if !temporary {
            if let Some(catalog) = self.storage.catalog.as_ref() {
                if !catalog
                    .replace_sequence_row(
                        &Self::sequence_row(&name, state, persistence)
                            .map_err(|err| format!("build sequence catalog row: {err}"))?,
                    )
                    .map_err(|err| format!("persist sequence catalog: {err}"))?
                {
                    return Err(format!("Sequence `{name}` does not exist"));
                }
            }
        }
        self.durable.sequences.write().insert(relation, state);
        self.note_catalog_registry_changed();
        Ok(true)
    }

    pub(crate) fn restart_owned_sequence(&self, name: &str) -> StorageBackendResult<()> {
        self.alter_sequence_inner(name, SequenceRestart::FromStart, None, None, false)
            .map(|_| ())
            .map_err(StorageBackendError::Other)
    }

    fn validate_sequence_increment(name: &str, increment: i64) -> Result<(), String> {
        if increment == 0 {
            Err(format!("Sequence `{name}` increment must not be zero"))
        } else {
            Ok(())
        }
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
        let relation = Self::resolved_relation_identity(&name)
            .map_err(|err| format!("resolve sequence `{name}`: {err}"))?;
        let temporary = self
            .durable
            .sequence_persistence
            .read()
            .get(&relation)
            .is_some_and(|persistence| {
                *persistence == uqa_sql::ast::RelationPersistence::Temporary
            });
        self.ensure_no_sequence_default_dependencies(&name)
            .map_err(|err| format!("DROP SEQUENCE `{name}` rejected: {err}"))?;
        let dependent_views = self
            .views_depending_on_sequence(&name)
            .map_err(|err| format!("DROP SEQUENCE `{name}` dependency scan failed: {err}"))?;
        if !dependent_views.is_empty() {
            return Err(format!(
                "DROP SEQUENCE `{name}` rejected: dependent view(s) `{}` reference it",
                dependent_views.join("`, `")
            ));
        }
        let removed = if temporary {
            self.durable.sequences.read().contains_key(&relation)
        } else if let Some(catalog) = self.storage.catalog.as_ref() {
            catalog
                .drop_sequence_row(&name)
                .map_err(|err| format!("persist sequence catalog: {err}"))?
        } else {
            self.durable.sequences.read().contains_key(&relation)
        };
        if removed {
            self.durable.sequences.write().remove(&relation);
            self.durable.sequence_persistence.write().remove(&relation);
            self.session
                .state
                .write()
                .sequence_currvals
                .remove(&relation);
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

    fn resolve_sequence_value_target(
        &self,
        reference: &str,
    ) -> Result<(String, RelationIdentity), SequenceValueError> {
        let name = self
            .try_resolve_sequence_name(reference)
            .map_err(|error| {
                SequenceValueError::Internal(format!("load sequence catalog: {error}"))
            })?
            .ok_or_else(|| SequenceValueError::Undefined(reference.to_string()))?;
        let relation = Self::resolved_relation_identity(&name).map_err(|error| {
            SequenceValueError::Internal(format!("resolve sequence `{name}`: {error}"))
        })?;
        Ok((name, relation))
    }

    pub fn nextval(&self, name: &str) -> Result<i64, String> {
        self.nextval_inner(name).map_err(|error| error.to_string())
    }

    pub(crate) fn nextval_sql(&self, name: &str) -> Result<i64, SQLError> {
        self.nextval_inner(name)
            .map_err(SequenceValueError::into_sql_error)
    }

    fn nextval_inner(&self, name: &str) -> Result<i64, SequenceValueError> {
        let (name, relation) = self.resolve_sequence_value_target(name)?;
        let previous = self
            .durable
            .sequences
            .read()
            .get(&relation)
            .copied()
            .ok_or_else(|| SequenceValueError::Undefined(name.clone()))?;
        let temporary = self
            .durable
            .sequence_persistence
            .read()
            .get(&relation)
            .is_some_and(|persistence| {
                *persistence == uqa_sql::ast::RelationPersistence::Temporary
            });
        let sequence_session = if temporary {
            None
        } else {
            self.open_nontransactional_sequence_session()
                .map_err(|error| {
                    SequenceValueError::Internal(format!("open sequence session: {error}"))
                })?
        };
        if !temporary && sequence_session.is_none() {
            self.prepare_explicit_transaction_writer()
                .map_err(|error| {
                    SequenceValueError::Internal(format!("prepare sequence writer: {error}"))
                })?;
        }
        let catalog = (!temporary)
            .then(|| {
                sequence_session
                    .as_ref()
                    .map(|session| session.catalog.as_ref())
                    .or(self.storage.catalog.as_deref())
            })
            .flatten();
        let current = if let Some(catalog) = catalog {
            match catalog.next_sequence_value(&name) {
                Ok(Some(current)) => current,
                Ok(None) => return Err(SequenceValueError::Undefined(name)),
                Err(_)
                    if previous.called
                        && previous.current.checked_add(previous.increment).is_none() =>
                {
                    return Err(SequenceValueError::exhausted(&name, previous));
                }
                Err(error) => {
                    return Err(SequenceValueError::Internal(format!(
                        "allocate sequence value: {error}"
                    )));
                }
            }
        } else {
            let mut seqs = self.durable.sequences.write();
            let seq = seqs
                .get_mut(&relation)
                .ok_or_else(|| SequenceValueError::Undefined(name.clone()))?;
            if seq.called {
                seq.current = seq
                    .current
                    .checked_add(seq.increment)
                    .ok_or_else(|| SequenceValueError::exhausted(&name, *seq))?;
            } else {
                seq.called = true;
            }
            seq.current
        };
        if let Some(state) = self.durable.sequences.write().get_mut(&relation) {
            state.current = current;
            state.called = true;
        }
        self.session
            .state
            .write()
            .sequence_currvals
            .insert(relation, current);
        Ok(current)
    }

    pub fn currval(&self, name: &str) -> Result<i64, String> {
        self.currval_inner(name).map_err(|error| error.to_string())
    }

    pub(crate) fn currval_sql(&self, name: &str) -> Result<i64, SQLError> {
        self.currval_inner(name)
            .map_err(SequenceValueError::into_sql_error)
    }

    fn currval_inner(&self, name: &str) -> Result<i64, SequenceValueError> {
        let (name, relation) = self.resolve_sequence_value_target(name)?;
        self.session
            .state
            .read()
            .sequence_currvals
            .get(&relation)
            .copied()
            .ok_or(SequenceValueError::CurrvalUndefined(name))
    }

    pub fn setval(&self, name: &str, value: i64) -> Result<i64, String> {
        self.setval_inner(name, value)
            .map_err(|error| error.to_string())
    }

    pub(crate) fn setval_sql(&self, name: &str, value: i64) -> Result<i64, SQLError> {
        self.setval_inner(name, value)
            .map_err(SequenceValueError::into_sql_error)
    }

    fn setval_inner(&self, name: &str, value: i64) -> Result<i64, SequenceValueError> {
        let (name, relation) = self.resolve_sequence_value_target(name)?;
        let temporary = self
            .durable
            .sequence_persistence
            .read()
            .get(&relation)
            .is_some_and(|persistence| {
                *persistence == uqa_sql::ast::RelationPersistence::Temporary
            });
        let sequence_session = if temporary {
            None
        } else {
            self.open_nontransactional_sequence_session()
                .map_err(|error| {
                    SequenceValueError::Internal(format!("open sequence session: {error}"))
                })?
        };
        if !temporary && sequence_session.is_none() {
            self.prepare_explicit_transaction_writer()
                .map_err(|error| {
                    SequenceValueError::Internal(format!("prepare sequence writer: {error}"))
                })?;
        }
        let catalog = if temporary {
            None
        } else {
            sequence_session
                .as_ref()
                .map(|session| session.catalog.as_ref())
                .or(self.storage.catalog.as_deref())
        };
        if let Some(catalog) = catalog {
            catalog
                .set_sequence_value(&name, value)
                .map_err(|error| {
                    SequenceValueError::Internal(format!("persist sequence value: {error}"))
                })?
                .ok_or_else(|| SequenceValueError::Undefined(name.clone()))?;
        }
        let mut seqs = self.durable.sequences.write();
        let seq = seqs
            .get_mut(&relation)
            .ok_or(SequenceValueError::Undefined(name))?;
        seq.current = value;
        seq.called = true;
        drop(seqs);
        self.session
            .state
            .write()
            .sequence_currvals
            .insert(relation, value);
        Ok(value)
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
        state: SequenceState,
        persistence: uqa_sql::ast::RelationPersistence,
    ) -> StorageBackendResult<SequenceRow> {
        Ok(SequenceRow {
            relation: RelationIdentity::from_legacy_name(name)
                .map_err(StorageBackendError::Other)?,
            start: state.start,
            increment: state.increment,
            current: state.current,
            called: state.called,
            persistence: persistence.catalog_code().into(),
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

    fn open_nontransactional_sequence_session(
        &self,
    ) -> StorageBackendResult<Option<uqa_storage::PersistentStorageSession>> {
        if !self.backend_transaction_is_deferred()
            || self.session.row_lock_statements.lock().is_empty()
        {
            return Ok(None);
        }
        self.storage
            .provider
            .as_ref()
            .map_or(Ok(None), |provider| provider.open_session().map(Some))
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
                        state,
                        uqa_sql::ast::RelationPersistence::Permanent,
                    )?)?;
                }
                catalog.set_metadata(SEQUENCES_METADATA_KEY, "{}")?;
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
        let mut persistence = temporary_persistence;
        for row in rows {
            let name = row.relation.qualified_name();
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
            sequences.insert(relation, state);
        }
        *self.durable.sequences.write() = sequences;
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
        Ok((
            row.relation,
            SequenceState {
                start: row.start,
                increment: row.increment,
                current: row.current,
                called: row.called,
            },
        ))
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
