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

    /// `LOAD 'library'`. The engine embeds the extension surfaces it
    /// supports, so loading Apache AGE succeeds as a no-op through every
    /// spelling `PostgreSQL` resolves against `$libdir`; any other library
    /// fails exactly like a missing shared object.
    pub fn load_library(&self, library: &str) -> Result<(), SQLError> {
        let requested = library.strip_prefix("$libdir/").unwrap_or(library);
        let base = requested.strip_suffix(".so").unwrap_or(requested);
        if base == "age" && !requested.contains('/') {
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
                schemas.contains(name.as_str())
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
            if (schemas.contains(name.as_str())
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
        if !crate::is_known_runtime_parameter(name) {
            return Err(SQLError::Routine {
                sqlstate: "42704".into(),
                message: format!("unrecognized configuration parameter \"{name}\""),
            });
        }
        if !crate::is_mutable_runtime_parameter(name) {
            return Err(SQLError::Routine {
                sqlstate: "55P02".into(),
                message: format!("parameter \"{name}\" cannot be changed"),
            });
        }
        if name.eq_ignore_ascii_case("work_mem") {
            Self::parse_work_mem_bytes(value)?;
        }
        if name.eq_ignore_ascii_case("search_path") {
            let parts = parse_search_path_list(value)?;
            let mut session = self.session.state.write();
            session.search_path = if parts.is_empty() {
                vec!["public".to_string()]
            } else {
                parts
            };
            session
                .session_vars
                .insert(name.to_string(), value.to_string());
            session.sql_statement_cache.clear();
            return Ok(());
        }
        self.session
            .state
            .write()
            .session_vars
            .insert(name.to_string(), value.to_string());
        Ok(())
    }

    /// Read back a session variable. `search_path` always resolves to
    /// the current resolution order; every other key looks up the
    /// session-vars map, then PostgreSQL-compatible runtime defaults,
    /// and finally the registered runtime default. Unknown parameters are
    /// errors rather than successful empty strings.
    pub fn show_variable(&self, name: &str) -> Result<String, SQLError> {
        if name.eq_ignore_ascii_case("search_path") {
            return Ok(self.search_path().join(","));
        }
        let session = self.session.state.read();
        if let Some(value) = session.session_vars.get(name) {
            return Ok(value.clone());
        }
        if let Some((_, value)) = session
            .session_vars
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
        {
            return Ok(value.clone());
        }
        crate::default_runtime_parameter(name)
            .map(str::to_string)
            .ok_or_else(|| SQLError::Routine {
                sqlstate: "42704".into(),
                message: format!("unrecognized configuration parameter \"{name}\""),
            })
    }

    pub(crate) fn work_mem_bytes(&self) -> Result<usize, SQLError> {
        Self::parse_work_mem_bytes(&self.show_variable("work_mem")?)
    }

    fn parse_work_mem_bytes(raw: &str) -> Result<usize, SQLError> {
        let compact = raw
            .trim()
            .chars()
            .filter(|character| !character.is_ascii_whitespace())
            .collect::<String>();
        let digits = compact.bytes().take_while(u8::is_ascii_digit).count();
        if digits == 0 {
            return Err(SQLError::TypeMismatch(format!(
                "work_mem must be a positive byte size, got {raw:?}"
            )));
        }
        let amount = compact[..digits].parse::<usize>().map_err(|_| {
            SQLError::TypeMismatch(format!("work_mem is outside the supported range: {raw:?}"))
        })?;
        if amount == 0 {
            return Err(SQLError::TypeMismatch(
                "work_mem must be greater than zero".into(),
            ));
        }
        let unit = compact[digits..].to_ascii_lowercase();
        let exponent = match unit.as_str() {
            // PostgreSQL interprets a bare work_mem integer as kilobytes.
            "b" => 0,
            "" | "k" | "kb" | "kib" => 1,
            "m" | "mb" | "mib" => 2,
            "g" | "gb" | "gib" => 3,
            "t" | "tb" | "tib" => 4,
            _ => {
                return Err(SQLError::TypeMismatch(format!(
                    "unsupported work_mem unit in {raw:?}"
                )))
            }
        };
        let multiplier = 1024_usize.checked_pow(exponent).ok_or_else(|| {
            SQLError::TypeMismatch(format!("work_mem is outside the supported range: {raw:?}"))
        })?;
        amount.checked_mul(multiplier).ok_or_else(|| {
            SQLError::TypeMismatch(format!("work_mem is outside the supported range: {raw:?}"))
        })
    }

    /// Apply `DISCARD <target>`. `ALL` resets every kind of session state;
    /// the narrower
    /// variants are scoped accordingly.
    pub fn discard(&self, target: uqa_sql::ast::DiscardTarget) -> Result<(), SQLError> {
        use uqa_sql::ast::DiscardTarget;
        let _statement = self.runtime.statement_gate.lock();
        if self.transaction_depth() != 0 {
            return Err(SQLError::Routine {
                sqlstate: "25001".into(),
                message: "DISCARD cannot run inside a transaction block".into(),
            });
        }
        if matches!(target, DiscardTarget::All | DiscardTarget::Temp) {
            self.discard_temporary_relations();
        }
        let mut session = self.session.state.write();
        match target {
            DiscardTarget::All => {
                session.session_vars.clear();
                session.prepared.clear();
                session.sequence_currvals.clear();
                session.sql_statement_cache.clear();
                session.search_path = vec!["public".to_string()];
            }
            DiscardTarget::Plans => {
                session.prepared.clear();
                session.sql_statement_cache.clear();
            }
            DiscardTarget::Sequences => {
                session.sequence_currvals.clear();
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
            .sequence_persistence
            .write()
            .retain(|relation, _| !temporary_sequences.contains(relation));
        self.session
            .state
            .write()
            .sequence_currvals
            .retain(|relation, _| !temporary_sequences.contains(relation));
        if !temporary_tables.is_empty() {
            self.note_table_catalog_changed();
        }
        self.note_catalog_registry_changed();
    }
}
