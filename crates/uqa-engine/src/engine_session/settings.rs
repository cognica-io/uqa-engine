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

    /// First existing schema on this logical session's explicit search path.
    pub fn current_schema_name(&self) -> StorageBackendResult<Option<String>> {
        self.synchronize_catalog_registries()?;
        let session = self.session.state.read();
        let schemas = self.durable.schemas.read();
        Ok(session
            .search_path
            .iter()
            .find(|name| schemas.contains(name.as_str()))
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
        let path = &session.search_path;
        let mut out = Vec::new();
        if include_implicit && !path.iter().any(|name| name == "pg_catalog") {
            out.push("pg_catalog".to_string());
        }
        for name in path {
            if (schemas.contains(name.as_str())
                || matches!(name.as_str(), "pg_catalog" | "information_schema"))
                && !out.contains(name)
            {
                out.push(name.clone());
            }
        }
        Ok(out)
    }

    /// Draw one value in `[0, 1)` from this logical session's PRNG.
    pub fn next_random_value(&self) -> f64 {
        let mut session = self.session.state.write();
        let mut value = session.random_state;
        // xorshift64*; every stored state is non-zero.
        value ^= value >> 12;
        value ^= value << 25;
        value ^= value >> 27;
        session.random_state = value;
        let sample = value.wrapping_mul(0x2545_f491_4f6c_dd1d) >> 11;
        sample as f64 * (1.0 / ((1_u64 << 53) as f64))
    }

    /// Reseed this logical session's PRNG. Equal seeds produce equal streams
    /// without changing any sibling session.
    pub fn set_random_seed(&self, seed: f64) -> Result<(), String> {
        if !seed.is_finite() || !(-1.0..=1.0).contains(&seed) {
            return Err(format!(
                "setseed parameter {seed} is out of allowed range [-1,1]"
            ));
        }
        let normalized = if seed == 0.0 { 0 } else { seed.to_bits() };
        let mut state = normalized ^ 0x9e37_79b9_7f4a_7c15;
        // SplitMix64 avalanche so nearby floating-point seeds do not create
        // correlated xorshift states.
        state = (state ^ (state >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        state = (state ^ (state >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        state ^= state >> 31;
        if state == 0 {
            state = 0x2545_f491_4f6c_dd1d;
        }
        self.session.state.write().random_state = state;
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
        if target == DiscardTarget::Temp {
            return Err(SQLError::Unsupported(
                "DISCARD TEMP requires temporary-table support".to_string(),
            ));
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
            DiscardTarget::Temp => unreachable!("DISCARD TEMP returned before locking state"),
        }
        Ok(())
    }
}
