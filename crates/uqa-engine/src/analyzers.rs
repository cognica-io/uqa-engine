//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{
    analyzer_registry, normalize_analyzer_phase, parse_analyzer_config, Analyzer, AnalyzerPhase,
    Arc, Engine, TableState,
};
use uqa_sql::ast::ColumnType;

impl Engine {
    pub(crate) fn resolve_analyzer(&self, name: &str) -> std::result::Result<Analyzer, String> {
        let name = name.trim();
        if name.is_empty() {
            return Err("analyzer name cannot be empty".into());
        }
        if let Ok(analyzer) = analyzer_registry::get_analyzer(name) {
            return Ok(analyzer);
        }
        let Some(config_json) = self.durable.named_analyzers.read().get(name).cloned() else {
            return Err(format!("analyzer `{name}` is not registered"));
        };
        parse_analyzer_config(name, &config_json)
    }

    pub fn register_named_analyzer(
        &self,
        name: &str,
        config_json: &str,
    ) -> std::result::Result<(), String> {
        self.with_implicit_string_transaction(|engine| {
            engine.register_named_analyzer_inner(name, config_json)
        })
    }

    fn register_named_analyzer_inner(
        &self,
        name: &str,
        config_json: &str,
    ) -> std::result::Result<(), String> {
        self.synchronize_catalog_registries()
            .map_err(|err| format!("refresh analyzer catalog: {err}"))?;
        parse_analyzer_config(name, config_json)?;
        let mut analyzers = self.durable.named_analyzers.write();
        if let Some(catalog) = self.storage.catalog.as_ref() {
            catalog
                .save_analyzer(name, config_json)
                .map_err(|err| format!("persist analyzer `{name}`: {err}"))?;
        }
        analyzers.insert(name.to_string(), config_json.to_string());
        drop(analyzers);
        self.note_catalog_registry_changed();
        Ok(())
    }

    pub fn drop_named_analyzer(&self, name: &str) -> Result<bool, String> {
        self.with_implicit_string_transaction(|engine| engine.drop_named_analyzer_inner(name))
    }

    fn drop_named_analyzer_inner(&self, name: &str) -> Result<bool, String> {
        self.synchronize_catalog_registries()
            .map_err(|err| format!("refresh analyzer catalog: {err}"))?;
        if self
            .durable
            .table_field_analyzers
            .read()
            .values()
            .any(|(analyzer, _)| analyzer == name)
        {
            return Err(format!(
                "analyzer `{name}` is still assigned to a table field"
            ));
        }
        let mut analyzers = self.durable.named_analyzers.write();
        if !analyzers.contains_key(name) {
            return Ok(false);
        }
        if let Some(catalog) = self.storage.catalog.as_ref() {
            catalog
                .drop_analyzer(name)
                .map_err(|err| format!("drop analyzer `{name}`: {err}"))?;
        }
        let removed = analyzers.remove(name).is_some();
        drop(analyzers);
        if removed {
            self.note_catalog_registry_changed();
        }
        Ok(removed)
    }

    pub fn list_named_analyzers(&self) -> Result<Vec<String>, String> {
        self.synchronize_catalog_registries()
            .map_err(|err| format!("refresh analyzer catalog: {err}"))?;
        let mut names: Vec<String> = self
            .durable
            .named_analyzers
            .read()
            .keys()
            .cloned()
            .collect();
        names.sort();
        Ok(names)
    }

    pub fn set_table_field_analyzer(
        &self,
        table: &str,
        field: &str,
        analyzer_name: &str,
        phase: &str,
    ) -> std::result::Result<(), String> {
        self.with_implicit_string_transaction(|engine| {
            engine.set_table_field_analyzer_inner(table, field, analyzer_name, phase)
        })
    }

    fn set_table_field_analyzer_inner(
        &self,
        table: &str,
        field: &str,
        analyzer_name: &str,
        phase: &str,
    ) -> std::result::Result<(), String> {
        self.synchronize_catalog_registries()
            .map_err(|err| format!("refresh analyzer catalog: {err}"))?;
        let table_name = self
            .try_resolve_table_name(table)
            .map_err(|err| format!("resolve table `{table}`: {err}"))?
            .ok_or_else(|| format!("set_table_analyzer: table `{table}` does not exist"))?;
        let Some(t) = self
            .try_table(&table_name)
            .map_err(|err| format!("resolve table `{table}`: {err}"))?
        else {
            return Err(format!(
                "set_table_analyzer: table `{table}` does not exist"
            ));
        };
        Self::validate_table_analyzer_field(&table_name, &t, field)?;
        let analyzer = self.resolve_analyzer(analyzer_name)?;
        let (phase_name, phase) = normalize_analyzer_phase(phase)?;
        let (old_index, old_search) = {
            let index = t.inverted_index.read();
            (
                index.get_field_analyzer(field),
                index.get_search_analyzer(field),
            )
        };
        let rebuild = matches!(phase, AnalyzerPhase::Index | AnalyzerPhase::Both)
            && t.fts_fields().iter().any(|f| f == field);
        {
            let mut index = t.inverted_index.write();
            index
                .set_field_analyzer(field, analyzer, phase)
                .map_err(|e| format!("set_table_analyzer: {e}"))?;
        }
        if rebuild {
            if let Err(err) = Self::rebuild_fts_index(&t) {
                return Err(Self::restore_analyzer_error(
                    &t, field, old_index, old_search, true, err,
                ));
            }
        }
        if let Some(catalog) = self.storage.catalog.as_ref() {
            if let Err(err) =
                catalog.replace_table_field_analyzer(&table_name, field, &phase_name, analyzer_name)
            {
                return Err(Self::restore_analyzer_error(
                    &t,
                    field,
                    old_index,
                    old_search,
                    rebuild,
                    format!("persist table analyzer `{table_name}`.`{field}`: {err}"),
                ));
            }
        }
        self.durable.table_field_analyzers.write().insert(
            (table_name, field.to_string()),
            (analyzer_name.to_string(), phase_name),
        );
        if self.is_persistent() {
            self.note_table_catalog_changed();
            self.note_catalog_registry_changed();
        }
        Ok(())
    }

    /// A per-field analyzer is meaningful only for a real text column that is
    /// already part of the table's physical FTS index.  Enforce the same
    /// invariant both when accepting a new assignment and while hydrating
    /// persisted assignments on reopen.
    pub(crate) fn validate_table_analyzer_field(
        table_name: &str,
        table: &Arc<TableState>,
        field: &str,
    ) -> Result<(), String> {
        let column_type = table
            .columns
            .read()
            .iter()
            .find(|column| column.name == field)
            .map(|column| column.ty.clone())
            .ok_or_else(|| {
                format!("set_table_analyzer: column `{table_name}`.`{field}` does not exist")
            })?;
        if column_type != ColumnType::Text {
            return Err(format!(
                "set_table_analyzer: column `{table_name}`.`{field}` must be TEXT, got {column_type:?}"
            ));
        }
        if !table
            .fts_fields()
            .iter()
            .any(|candidate| candidate == field)
        {
            return Err(format!(
                "set_table_analyzer: field `{table_name}`.`{field}` is not registered in the physical FTS index"
            ));
        }
        Ok(())
    }

    fn restore_analyzer_error(
        table: &std::sync::Arc<super::TableState>,
        field: &str,
        index_analyzer: Analyzer,
        search_analyzer: Analyzer,
        rebuild: bool,
        original: String,
    ) -> String {
        match Self::restore_field_analyzers(table, field, index_analyzer, search_analyzer, rebuild)
        {
            Ok(()) => original,
            Err(cleanup) => {
                format!("{original}; restoring the prior field analyzer also failed: {cleanup}")
            }
        }
    }

    fn restore_field_analyzers(
        table: &std::sync::Arc<super::TableState>,
        field: &str,
        index_analyzer: Analyzer,
        search_analyzer: Analyzer,
        rebuild: bool,
    ) -> Result<(), String> {
        {
            let mut index = table.inverted_index.write();
            index.set_field_analyzer(field, index_analyzer, AnalyzerPhase::Index)?;
            index.set_field_analyzer(field, search_analyzer, AnalyzerPhase::Search)?;
        }
        if rebuild {
            Self::rebuild_fts_index(table)?;
        }
        Ok(())
    }

    pub fn table_field_analyzer(
        &self,
        table: &str,
        field: &str,
    ) -> Result<Option<(String, String)>, String> {
        self.synchronize_catalog_registries()
            .map_err(|err| format!("refresh analyzer catalog: {err}"))?;
        let Some(table) = self
            .try_resolve_table_name(table)
            .map_err(|err| format!("resolve table `{table}`: {err}"))?
        else {
            return Ok(None);
        };
        Ok(self
            .durable
            .table_field_analyzers
            .read()
            .get(&(table, field.to_string()))
            .cloned())
    }

    /// compatibility alias for [`Engine::register_named_analyzer`].
    pub fn create_analyzer(
        &self,
        name: &str,
        config_json: &str,
    ) -> std::result::Result<(), String> {
        self.register_named_analyzer(name, config_json)
    }

    /// compatibility alias for [`Engine::drop_named_analyzer`].
    pub fn drop_analyzer(&self, name: &str) -> Result<bool, String> {
        self.drop_named_analyzer(name)
    }

    /// compatibility alias for [`Engine::set_table_field_analyzer`].
    pub fn set_table_analyzer(
        &self,
        table: &str,
        field: &str,
        analyzer_name: &str,
        phase: &str,
    ) -> std::result::Result<(), String> {
        self.set_table_field_analyzer(table, field, analyzer_name, phase)
    }

    /// Resolve the analyzer assigned to `(table, field)` for the given
    /// phase. `phase` is `"index"`, `"search"`, or `"both"`. Returns the
    /// analyzer config JSON in the raw persisted form.
    pub fn get_table_analyzer(
        &self,
        table: &str,
        field: &str,
        phase: &str,
    ) -> Result<Option<String>, String> {
        let Some((name, stored_phase)) = self.table_field_analyzer(table, field)? else {
            return Ok(None);
        };
        // Resolve the field's index/search analyzer based on the requested
        // phase; "both" means the override applies on both sides.
        let resolved = match (stored_phase.as_str(), phase) {
            ("both", _) | ("index", "index") | ("query" | "search", "search") => name,
            _ => return Ok(None),
        };
        let analyzer = self.resolve_analyzer(&resolved)?;
        serde_json::to_string(&analyzer)
            .map(Some)
            .map_err(|err| format!("serialize analyzer `{resolved}`: {err}"))
    }
}
