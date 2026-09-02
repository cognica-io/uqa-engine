//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Logical-session search path, PRNG, runtime variables, and DISCARD.

use super::{parse_search_path_list, Engine, SQLError, StorageBackendResult};

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

    /// Whether `schema` is an explicit entry of the current `search_path`.
    pub(crate) fn search_path_contains(&self, schema: &str) -> bool {
        self.session
            .state
            .read()
            .search_path
            .iter()
            .any(|entry| entry == schema)
    }

    /// First existing namespace on this logical session's explicit search
    /// path: a durable schema, a virtual system schema such as `ag_catalog`,
    /// or a graph namespace.
    pub fn current_schema_name(&self) -> StorageBackendResult<Option<String>> {
        self.synchronize_catalog_registries()?;
        let session = self.session.state.read();
        let schemas = self.durable.schemas.read();
        let graphs = self.durable.graphs.read();
        Ok(session
            .search_path
            .iter()
            .find(|name| {
                schemas.contains_key(name.as_str())
                    || super::schemas::is_virtual_system_schema(name)
                    || graphs.contains_key(name.as_str())
            })
            .cloned())
    }

    /// Existing schemas visible through this logical session's search path.
    /// `PostgreSQL` implicitly searches `pg_catalog` unless it is already named
    /// explicitly; the flag controls whether that implicit entry is returned.
    pub fn current_schema_names(
        &self,
        include_implicit: bool,
    ) -> StorageBackendResult<Vec<String>> {
        self.synchronize_catalog_registries()?;
        let session = self.session.state.read();
        let schemas = self.durable.schemas.read();
        let graphs = self.durable.graphs.read();
        let path = &session.search_path;
        let mut out = Vec::new();
        if include_implicit && !path.iter().any(|name| name == "pg_catalog") {
            out.push("pg_catalog".to_string());
        }
        for name in path {
            if (schemas.contains_key(name.as_str())
                || super::schemas::is_virtual_system_schema(name)
                || graphs.contains_key(name.as_str()))
                && !out.contains(name)
            {
                out.push(name.clone());
            }
        }
        Ok(out)
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

    /// Replace the `search_path`. Empty input falls back to `["public"]`.
    pub fn set_search_path(&self, path: Vec<String>) {
        let mut value = path;
        if value.is_empty() {
            value.push("public".to_string());
        }
        let mut session = self.session.state.write();
        session.search_path = value;
        session.sql_statement_cache.clear();
    }

    /// Apply `SET <name> [TO|=] <value>`. Honours `search_path`
    /// directly; every other parameter is stored in the session-vars
    /// map so a subsequent `SHOW <name>` can echo it back.
    pub fn set_variable(&self, name: &str, value: &str) -> Result<(), SQLError> {
        if !crate::engine_capabilities::is_known_runtime_parameter(name) {
            return Err(SQLError::Routine {
                sqlstate: "42704".into(),
                message: format!("unrecognized configuration parameter \"{name}\""),
            });
        }
        if !crate::engine_capabilities::is_mutable_runtime_parameter(name) {
            return Err(SQLError::Routine {
                sqlstate: "55P02".into(),
                message: format!("parameter \"{name}\" cannot be changed"),
            });
        }
        if name.eq_ignore_ascii_case("transaction_isolation")
            || name.eq_ignore_ascii_case("transaction_read_only")
            || name.eq_ignore_ascii_case("transaction_deferrable")
        {
            return self.set_transaction_parameter(name, value);
        }
        if name.eq_ignore_ascii_case("session_replication_role") {
            if !self.current_user_is_superuser() {
                return Err(SQLError::Routine {
                    sqlstate: "42501".into(),
                    message: "permission denied to set parameter \"session_replication_role\""
                        .into(),
                });
            }
            let value = match value.trim().to_ascii_lowercase().as_str() {
                "origin" => "origin",
                "replica" => "replica",
                "local" => "local",
                _ => {
                    return Err(SQLError::Routine {
                        sqlstate: "22023".into(),
                        message: format!(
                            "invalid value for parameter \"session_replication_role\": \"{value}\"\nHINT: Available values: origin, replica, local."
                        ),
                    })
                }
            };
            let mut session = self.session.state.write();
            session
                .session_vars
                .insert("session_replication_role".into(), value.into());
            session.sql_statement_cache.clear();
            return Ok(());
        }
        let mut value = Self::validate_default_transaction_parameter(name, value)?;
        if name.eq_ignore_ascii_case("plpgsql.check_asserts") {
            value = if crate::engine_capabilities::parse_boolean_runtime_parameter(name, &value)? {
                "on".into()
            } else {
                "off".into()
            };
        }
        if name.eq_ignore_ascii_case("work_mem") {
            crate::engine_capabilities::parse_work_mem_bytes(&value)?;
        }
        if name.eq_ignore_ascii_case("search_path") {
            let parts = parse_search_path_list(&value)?;
            let mut session = self.session.state.write();
            session.search_path = if parts.is_empty() {
                vec!["public".to_string()]
            } else {
                parts
            };
            session.session_vars.insert(name.to_string(), value);
            session.sql_statement_cache.clear();
            return Ok(());
        }
        self.session
            .state
            .write()
            .session_vars
            .insert(name.to_string(), value);
        Ok(())
    }

    pub fn reset_variable(&self, name: &str) -> Result<(), SQLError> {
        if !crate::engine_capabilities::is_known_runtime_parameter(name) {
            return Err(SQLError::Routine {
                sqlstate: "42704".into(),
                message: format!("unrecognized configuration parameter \"{name}\""),
            });
        }
        if name.eq_ignore_ascii_case("transaction_isolation")
            || name.eq_ignore_ascii_case("transaction_read_only")
            || name.eq_ignore_ascii_case("transaction_deferrable")
        {
            return Err(SQLError::Routine {
                sqlstate: "0A000".into(),
                message: format!("parameter \"{name}\" cannot be reset"),
            });
        }
        if name.eq_ignore_ascii_case("session_replication_role")
            && !self.current_user_is_superuser()
        {
            return Err(SQLError::Routine {
                sqlstate: "42501".into(),
                message: "permission denied to set parameter \"session_replication_role\"".into(),
            });
        }
        if !crate::engine_capabilities::is_mutable_runtime_parameter(name) {
            return Err(SQLError::Routine {
                sqlstate: "55P02".into(),
                message: format!("parameter \"{name}\" cannot be changed"),
            });
        }
        let mut session = self.session.state.write();
        session
            .session_vars
            .retain(|key, _| !key.eq_ignore_ascii_case(name));
        if name.eq_ignore_ascii_case("search_path") {
            session.search_path = vec!["public".into()];
        }
        session.sql_statement_cache.clear();
        Ok(())
    }

    pub fn reset_all_variables(&self) {
        let mut session = self.session.state.write();
        session.session_vars.clear();
        session.search_path = vec!["public".into()];
        session.sql_statement_cache.clear();
    }

    /// Read back a session variable. `search_path` always resolves to
    /// the current resolution order; every other key looks up the
    /// session-vars map, then PostgreSQL-compatible runtime defaults,
    /// and finally the registered runtime default. Unknown parameters are
    /// errors rather than successful empty strings.
    pub fn show_variable(&self, name: &str) -> Result<String, SQLError> {
        self.session_execution_view().show_variable(name)
    }

    pub(crate) fn work_mem_bytes(&self) -> Result<usize, SQLError> {
        self.query_runtime_view().work_mem_bytes()
    }

    pub(crate) fn session_replication_role_is_replica(&self) -> bool {
        self.session
            .state
            .read()
            .session_vars
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case("session_replication_role"))
            .is_some_and(|(_, value)| value == "replica")
    }

    pub(crate) fn plpgsql_asserts_enabled(&self) -> bool {
        self.session
            .state
            .read()
            .session_vars
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case("plpgsql.check_asserts"))
            .is_none_or(|(_, value)| value == "on")
    }

    /// Apply `DISCARD <target>`. `ALL` resets every kind of session state;
    /// the narrower
    /// variants are scoped accordingly.
    pub fn discard(&self, target: uqa_sql::ast::DiscardTarget) -> Result<(), SQLError> {
        use uqa_sql::ast::DiscardTarget;
        let _statement = self.runtime.statement_gate.lock();
        if self.in_explicit_transaction_block() {
            return Err(SQLError::Routine {
                sqlstate: "25001".into(),
                message: "DISCARD cannot run inside a transaction block".into(),
            });
        }
        if matches!(target, DiscardTarget::All | DiscardTarget::Temp) {
            self.discard_temporary_relations();
        }
        if matches!(target, DiscardTarget::All | DiscardTarget::Sequences) {
            self.session.sequence_caches.lock().clear();
        }
        let mut session = self.session.state.write();
        match target {
            DiscardTarget::All => {
                session.session_vars.clear();
                session.prepared.clear();
                session.sequence_currvals.clear();
                session.last_sequence = None;
                session.sql_statement_cache.clear();
                session.search_path = vec!["public".to_string()];
                let session_user = session.session_user.clone();
                session.current_user = session_user;
                drop(session);
                self.session.portals.lock().clear();
                return Ok(());
            }
            DiscardTarget::Plans => {
                session.prepared.clear();
                session.sql_statement_cache.clear();
            }
            DiscardTarget::Sequences => {
                session.sequence_currvals.clear();
                session.last_sequence = None;
            }
            DiscardTarget::Temp => {}
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
