//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Logical-session search path, PRNG, runtime variables, and DISCARD.

use super::{Engine, SQLError, StorageBackendResult};

impl Engine {
    /// Return the current `search_path`.
    pub fn search_path(&self) -> Vec<String> {
        self.session.state.read().search_path.clone()
    }

    /// `LOAD 'library'`. The engine embeds Apache AGE and PL/pgSQL, so loading either succeeds as a no-op through every spelling `PostgreSQL` resolves against `$libdir`; any other library fails exactly like a missing shared object.
    pub fn load_library(&self, library: &str) -> Result<(), SQLError> {
        let requested = library.strip_prefix("$libdir/").unwrap_or(library);
        let base = requested.strip_suffix(".so").unwrap_or(requested);
        if matches!(base, "age" | "plpgsql") && !requested.contains('/') {
            self.load_language(base);
            return Ok(());
        }
        let path = if library.contains('/') {
            library.to_string()
        } else {
            format!("$libdir/{library}")
        };
        Err(SQLError::Routine {
            sqlstate: "58P01".into(),
            message: format!("could not access file \"{path}\": No such file or directory"),
        })
    }

    /// Load the library of `language` into the session, which defines its parameters: `PL/pgSQL` is the only embedded language whose library defines any.
    pub(crate) fn load_language(&self, language: &str) {
        if language == "plpgsql" {
            self.load_parameter_library("plpgsql");
        }
    }

    /// First usable namespace on this logical session's explicit search path: a durable, virtual system or graph namespace.
    pub fn current_schema_name(&self) -> StorageBackendResult<Option<String>> {
        self.with_catalog_read_snapshot(Self::current_schema_name_in_execution)
    }

    pub(crate) fn current_schema_name_in_execution(&self) -> StorageBackendResult<Option<String>> {
        self.catalog_execution()
            .current_schema_name()
            .map_err(|error| uqa_storage::StorageBackendError::backend("schema namespace", error))
    }

    /// Existing schemas with USAGE privilege in this logical session's search path. `PostgreSQL` implicitly searches `pg_catalog` unless it is already named explicitly.
    pub fn current_schema_names(
        &self,
        include_implicit: bool,
    ) -> StorageBackendResult<Vec<String>> {
        self.with_catalog_read_snapshot(|engine| {
            engine.current_schema_names_in_execution(include_implicit)
        })
    }

    pub(crate) fn current_schema_names_in_execution(
        &self,
        include_implicit: bool,
    ) -> StorageBackendResult<Vec<String>> {
        self.catalog_execution()
            .current_schema_names(include_implicit)
            .map_err(|error| uqa_storage::StorageBackendError::backend("schema namespace", error))
    }

    /// Draw every bit of one word from this logical session's PRNG.
    pub fn next_random_u64(&self) -> u64 {
        let mut state = self.session.random_state.lock();
        let s0 = state.s0;
        let mixed = state.s1 ^ s0;
        let value = s0.wrapping_mul(5).rotate_left(7).wrapping_mul(9);
        state.s0 = s0.rotate_left(24) ^ mixed ^ (mixed << 16);
        state.s1 = mixed.rotate_left(37);
        value
    }

    /// Draw one value in `[0, 1)` from this logical session's PRNG.
    pub fn next_random_value(&self) -> f64 {
        let sample = self.next_random_u64() >> 12;
        sample as f64 * (1.0 / ((1_u64 << 52) as f64))
    }

    /// Reseed this logical session's PRNG. Equal seeds produce equal streams
    /// without changing any sibling session.
    pub fn set_random_seed(&self, seed: f64) -> Result<(), String> {
        if !seed.is_finite() || !(-1.0..=1.0).contains(&seed) {
            return Err(format!(
                "setseed parameter {seed} is out of allowed range [-1,1]"
            ));
        }
        let scaled = (((1_u64 << 52) - 1) as f64 * seed) as i64;
        *self.session.random_state.lock() = crate::random_state_from_seed(scaled as u64);
        Ok(())
    }

    /// Set the session's `search_path` to `path`, each schema quoted as an identifier needs, as `SET search_path TO` with those names does. An empty path finds unqualified names only in the system schemas.
    pub fn set_search_path(&self, path: &[String]) -> Result<(), SQLError> {
        let setting = path
            .iter()
            .map(|schema| uqa_sql::expr::quote_ident(schema))
            .collect::<Vec<_>>()
            .join(", ");
        self.set_runtime_parameter("search_path", Some(&setting), false)
    }

    /// Bound the memory each query workspace of this session may hold before it spills, in place of `work_mem`; `None` returns to `work_mem`. Unlike `work_mem`, whose minimum is `PostgreSQL`'s 64 kB, the bound may be as small as one byte, so that a host can make queries over little data spill.
    pub fn set_query_memory_limit(&self, bytes: Option<usize>) {
        self.session.query_memory_limit.store(
            bytes.map_or(0, |bytes| bytes.max(1)),
            std::sync::atomic::Ordering::Release,
        );
    }

    /// Apply `SET <name> [TO|=] <value>` for the session.
    pub fn set_variable(&self, name: &str, value: &str) -> Result<(), SQLError> {
        self.set_runtime_parameter(name, Some(value), false)
    }

    pub fn reset_variable(&self, name: &str) -> Result<(), SQLError> {
        self.set_runtime_parameter(name, None, false)
    }

    /// `RESET ALL`: every parameter except those `RESET ALL` leaves alone returns to the setting `RESET` restores.
    pub fn reset_all_variables(&self) {
        let search_path = self.reset_search_path();
        let mut session = self.session.state.write();
        session.session_vars.clear();
        session.parameter_scopes.reset_all();
        session.search_path = search_path;
        session.sql_statement_cache.clear();
    }

    /// The value of the parameter `name` as `SHOW` reports it; a name nothing defines is an error.
    pub fn show_variable(&self, name: &str) -> Result<String, SQLError> {
        self.session_execution_view().show_variable(name)
    }

    pub(crate) fn set_runtime_parameter(
        &self,
        name: &str,
        value: Option<&str>,
        local: bool,
    ) -> Result<(), SQLError> {
        use uqa_sql::semantics::parameters::ParameterAssignment;
        self.assign_runtime_parameter(
            name,
            value,
            if local {
                ParameterAssignment::Local
            } else {
                ParameterAssignment::Session
            },
        )
    }

    pub(crate) fn set_configured_parameter(&self, name: &str, value: &str) -> Result<(), SQLError> {
        self.assign_runtime_parameter(
            name,
            Some(value),
            uqa_sql::semantics::parameters::ParameterAssignment::Save,
        )
    }

    fn assign_runtime_parameter(
        &self,
        name: &str,
        value: Option<&str>,
        action: uqa_sql::semantics::parameters::ParameterAssignment,
    ) -> Result<(), SQLError> {
        use crate::state::RuntimeParameterValue;
        let key = crate::capabilities::session_parameters::parameter_key(name);
        let before = {
            let state = self.session.state.read();
            let setting = state.session_vars.get(&key).cloned();
            match key.as_str() {
                "search_path" => RuntimeParameterValue::SearchPath {
                    setting,
                    path: state.search_path.clone(),
                },
                "role" => RuntimeParameterValue::Role(state.authorization.selected().cloned()),
                "session_authorization" => RuntimeParameterValue::SessionAuthorization(
                    state.authorization.session().clone(),
                ),
                _ => RuntimeParameterValue::Setting(setting),
            }
        };
        let prior_role = (key == "session_authorization").then(|| {
            RuntimeParameterValue::Role(self.session.state.read().authorization.selected().cloned())
        });
        if key == "session_authorization" {
            uqa_execution::catalog::security::role_lifecycle::set_session_authorization(
                &self.role_execution_context(),
                value,
            )?;
        } else if key == "role" {
            uqa_execution::catalog::security::role_lifecycle::set_role(
                &self.role_execution_context(),
                value,
            )?;
        } else if let Some(value) = value {
            self.assign_parameter(name, value)?;
        } else {
            self.reset_parameter(name)?;
        }
        // Every SQL statement runs in a transaction, its own when no block is open, which ends the statement's LOCAL assignments.
        let in_transaction = self.transaction_depth() != 0
            || self
                .runtime
                .sql_execution_depth
                .load(std::sync::atomic::Ordering::Relaxed)
                != 0;
        let mut state = self.session.state.write();
        if let Some(prior_role) = prior_role {
            if let Some(previous) =
                state
                    .parameter_scopes
                    .assigned("role".into(), prior_role, action, in_transaction)
            {
                restore_runtime_parameter(&mut state, "role", previous);
            }
        }
        if let Some(previous) =
            state
                .parameter_scopes
                .assigned(key.clone(), before, action, in_transaction)
        {
            // A LOCAL assignment outside any transaction lasts only for its own statement; the statement executor reports the warning.
            restore_runtime_parameter(&mut state, &key, previous);
        }
        Ok(())
    }

    pub(crate) fn restore_local_runtime_parameters(&self) {
        let mut state = self.session.state.write();
        let saved = state.parameter_scopes.finish_transaction();
        for (name, value) in saved {
            restore_runtime_parameter(&mut state, &name, value);
        }
    }

    pub(crate) fn work_mem_bytes(&self) -> Result<usize, SQLError> {
        self.query_runtime_view().work_mem_bytes()
    }

    pub(crate) fn session_replication_role_is_replica(&self) -> bool {
        self.session.setting("session_replication_role") == "replica"
    }

    /// Whether a query may project the values that indexes hold instead of reading documents, as `PostgreSQL`'s `enable_indexonlyscan` does.
    pub(crate) fn index_only_scans_enabled(&self) -> bool {
        self.session.setting("enable_indexonlyscan") == "on"
    }

    /// Whether `ASSERT` checks its condition (`plpgsql.check_asserts`), which applies once the session has loaded `PL/pgSQL`, as running an `ASSERT` has.
    pub(crate) fn plpgsql_asserts_enabled(&self) -> bool {
        self.session.setting("plpgsql.check_asserts") == "on"
    }

    /// Apply `DISCARD <target>`. `ALL` resets every kind of session state;
    /// the narrower
    /// variants are scoped accordingly.
    pub fn discard(&self, target: uqa_sql::ast::DiscardTarget) -> Result<(), SQLError> {
        use uqa_sql::ast::DiscardTarget;
        let _statement = self.runtime.statement_gate.lock();
        if target == DiscardTarget::All && self.in_explicit_transaction_block() {
            return Err(SQLError::Routine {
                sqlstate: "25001".into(),
                message: "DISCARD ALL cannot run inside a transaction block".into(),
            });
        }
        if matches!(target, DiscardTarget::All | DiscardTarget::Temp) {
            self.discard_temporary_relations();
            if self.transaction_depth() == 0 {
                self.row_locks.close_temporary_roles(self.session_id);
            }
        }
        if matches!(target, DiscardTarget::All | DiscardTarget::Sequences) {
            self.discard_sequence_session_values();
        }
        if target == DiscardTarget::All {
            if self.transaction_depth() == 0 {
                self.clear_notification_listener_without_transaction()?;
            } else {
                self.unlisten(None)?;
            }
        }
        let search_path = self.reset_search_path();
        let mut session = self.session.state.write();
        match target {
            DiscardTarget::All => {
                session.session_vars.clear();
                session.search_path = search_path;
                self.session.prepared.write().clear();
                session.sql_statement_cache.clear();
                session.authorization.discard();
                drop(session);
                self.session.portals.lock().clear();
                return Ok(());
            }
            DiscardTarget::Plans => {
                self.invalidate_prepared_plans();
                session.sql_statement_cache.clear();
            }
            DiscardTarget::Sequences | DiscardTarget::Temp => {}
        }
        Ok(())
    }

    fn discard_temporary_relations(&self) {
        let schema = self.temporary_schema_name();
        let temporary_tables = self
            .storage
            .tables
            .read()
            .keys()
            .filter(|relation| relation.schema == schema)
            .cloned()
            .collect::<Vec<_>>();
        let temporary_table_names = temporary_tables
            .iter()
            .map(super::RelationIdentity::qualified_name)
            .collect::<std::collections::BTreeSet<_>>();
        self.storage
            .tables
            .write()
            .retain(|relation, _| relation.schema != schema);
        self.durable
            .table_field_analyzers
            .write()
            .retain(|(table, _), _| !temporary_table_names.contains(table));
        self.durable
            .catalog_indexes
            .write()
            .retain(|_, index| !temporary_table_names.contains(&index.table_name));
        self.durable
            .views
            .write()
            .retain(|relation, _| relation.schema != schema);
        let temporary_sequences = self
            .durable
            .sequence_persistence
            .read()
            .iter()
            .filter(|(relation, persistence)| {
                relation.schema == schema
                    && **persistence == uqa_sql::ast::RelationPersistence::Temporary
            })
            .map(|(relation, _)| relation.clone())
            .collect::<std::collections::BTreeSet<_>>();
        self.durable
            .sequences
            .write()
            .retain(|relation, _| !temporary_sequences.contains(relation));
        self.durable
            .sequence_object_ids
            .write()
            .retain(|relation, _| !temporary_sequences.contains(relation));
        self.durable
            .sequence_persistence
            .write()
            .retain(|relation, _| !temporary_sequences.contains(relation));
        self.durable
            .sequence_security
            .write()
            .retain(|relation, _| !temporary_sequences.contains(relation));
        let mut session = self.session.state.write();
        session
            .sequence_currvals
            .retain(|relation, _| !temporary_sequences.contains(relation));
        if session
            .last_sequence
            .as_ref()
            .is_some_and(|last| temporary_sequences.contains(&last.relation))
        {
            session.last_sequence = None;
        }
        drop(session);
        self.session
            .sequence_caches
            .lock()
            .retain(|relation, _| !temporary_sequences.contains(relation));
        if !temporary_tables.is_empty() {
            self.note_table_catalog_changed();
        }
        self.note_catalog_registry_changed();
    }
}

pub(crate) fn restore_runtime_parameter(
    state: &mut crate::SessionStateSnapshot,
    name: &str,
    value: crate::state::RuntimeParameterValue,
) {
    use crate::state::RuntimeParameterValue;
    let setting = match value {
        RuntimeParameterValue::Setting(value) => value,
        RuntimeParameterValue::SearchPath { setting, path } => {
            state.search_path = path;
            state.sql_statement_cache.clear();
            setting
        }
        RuntimeParameterValue::SessionAuthorization(role) => {
            state.authorization.restore_session(role);
            state.sql_statement_cache.clear();
            return;
        }
        RuntimeParameterValue::Role(role) => {
            state.authorization.set_role(role);
            state.sql_statement_cache.clear();
            return;
        }
    };
    state
        .session_vars
        .retain(|key, _| !key.eq_ignore_ascii_case(name));
    if let Some(value) = setting {
        state.session_vars.insert(name.into(), value);
    }
}
