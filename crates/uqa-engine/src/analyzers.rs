//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{
    analyzer_registry, normalize_analyzer_phase, parse_analyzer_config, AnalyzerPhase, Arc, Engine,
    TableState,
};
use uqa_sql::ast::ColumnType;
use uqa_storage::{AnalyzerBindingOwner, FieldAnalyzerBinding};

mod persistence;

impl Engine {
    pub(crate) fn resolve_analyzer_revision(
        &self,
        name: &str,
    ) -> Result<Arc<uqa_analysis::CompiledAnalyzer>, String> {
        let name = name.trim();
        if name.is_empty() {
            return Err("analyzer name cannot be empty".into());
        }
        if let Ok(analyzer) = analyzer_registry::get_analyzer(name) {
            return analyzer.compile().map_err(|error| error.to_string());
        }
        self.durable
            .named_analyzers
            .read()
            .get(name)
            .cloned()
            .ok_or_else(|| format!("analyzer `{name}` is not registered"))
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
        let name = name.trim();
        if analyzer_registry::is_builtin_analyzer(name) {
            return Err(format!("cannot overwrite built-in analyzer `{name}`"));
        }
        let compiled = parse_analyzer_config(name, config_json)?
            .compile()
            .map_err(|error| error.to_string())?;
        let configuration = serde_json::to_string(
            &compiled
                .descriptor()
                .configuration()
                .map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string())?;
        let mut analyzers = self.durable.named_analyzers.write();
        if let Some(catalog) = self.storage.catalog.as_ref() {
            catalog
                .save_analyzer_revision(
                    name,
                    &configuration,
                    compiled.descriptor().canonical_json(),
                )
                .map_err(|err| format!("persist analyzer `{name}`: {err}"))?;
        }
        analyzers.insert(name.to_owned(), compiled);
        drop(analyzers);
        self.note_catalog_registry_changed();
        Ok(())
    }

    /// Analyze one input with a resolved revision and return the SQL diagnostic object.
    pub fn analyze_text(&self, name: &str, input: &str) -> Result<uqa_core::Value, String> {
        self.synchronize_catalog_registries()
            .map_err(|err| format!("refresh analyzer catalog: {err}"))?;
        let analyzer = self.resolve_analyzer_revision(name)?;
        let runtime = self.query_runtime_view();
        let budget = uqa_core::memory::MemoryBudget::new(
            runtime
                .work_mem_bytes()
                .map_err(|error| error.to_string())?,
        );
        let mut poll = || {
            runtime
                .cancellation
                .check()
                .map_err(|_| uqa_analysis::AnalysisError::Cancelled)
        };
        let analysis = analyzer
            .analyze_tokens_budgeted(input, &budget, &mut poll)
            .map_err(|error| format!("analyze text with `{}`: {error}", name.trim()))?;
        let analysis = analysis.into_parts().0;
        let mut diagnostic = serde_json::to_value(&analysis)
            .map_err(|error| format!("serialize analysis for `{}`: {error}", name.trim()))?;
        let object = diagnostic
            .as_object_mut()
            .ok_or_else(|| "analysis diagnostic must be a JSON object".to_owned())?;
        object.insert(
            "analyzer_fingerprint".into(),
            serde_json::to_value(analyzer.descriptor().fingerprint())
                .map_err(|error| format!("serialize analyzer fingerprint: {error}"))?,
        );
        let json = serde_json::to_string(&diagnostic)
            .map_err(|error| format!("serialize analysis diagnostic: {error}"))?;
        Ok(uqa_core::Value::JsonB(json))
    }

    pub fn drop_named_analyzer(&self, name: &str) -> Result<bool, String> {
        self.with_implicit_string_transaction(|engine| engine.drop_named_analyzer_inner(name))
    }

    fn drop_named_analyzer_inner(&self, name: &str) -> Result<bool, String> {
        let name = name.trim();
        self.synchronize_catalog_registries()
            .map_err(|err| format!("refresh analyzer catalog: {err}"))?;
        if self
            .durable
            .table_field_analyzers
            .read()
            .values()
            .any(|binding| binding.uses_name(name))
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
        let analyzer_name = analyzer_name.trim();
        let analyzer = self.resolve_analyzer_revision(analyzer_name)?;
        let (_, phase) = normalize_analyzer_phase(phase)?;
        let (old_index, old_search) = {
            let index = t.inverted_index.read();
            (
                index
                    .index_analyzer_revision(field)
                    .map_err(|error| format!("resolve prior index analyzer: {error}"))?,
                index
                    .search_analyzer_revision(field)
                    .map_err(|error| format!("resolve prior search analyzer: {error}"))?,
            )
        };
        let previous = self
            .durable
            .table_field_analyzers
            .read()
            .get(&(table_name.clone(), field.to_owned()))
            .cloned()
            .unwrap_or_else(|| {
                FieldAnalyzerBinding::unassigned(old_index.clone(), old_search.clone())
            });
        if previous.owner == AnalyzerBindingOwner::Gin {
            return Err(format!("field `{table_name}`.`{field}` has a GIN-owned analyzer; recreate its owning index before assigning a field analyzer"));
        }
        let candidate = previous.assigned(
            analyzer_name,
            analyzer.clone(),
            phase,
            AnalyzerBindingOwner::Field,
        );
        let rebuild = matches!(phase, AnalyzerPhase::Index | AnalyzerPhase::Both)
            && t.fts_fields().iter().any(|f| f == field);
        if rebuild {
            let documents = Self::project_fts_sources(&t)?;
            t.inverted_index
                .write()
                .rebuild_with_analyzer_revision(field, analyzer, phase, documents)
                .map_err(|error| format!("set_table_analyzer: {error}"))?;
        } else {
            t.inverted_index
                .write()
                .set_field_analyzer_revision(field, analyzer, phase)
                .map_err(|error| format!("set_table_analyzer: {error}"))?;
        }
        if let Err(error) = self.persist_field_analyzer_binding(&table_name, field, &candidate) {
            return Err(Self::restore_analyzer_error(
                &t, field, old_index, old_search, rebuild, error,
            ));
        }
        self.durable
            .table_field_analyzers
            .write()
            .insert((table_name, field.to_owned()), candidate);
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
        index_analyzer: Arc<uqa_analysis::CompiledAnalyzer>,
        search_analyzer: Arc<uqa_analysis::CompiledAnalyzer>,
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
        index_analyzer: Arc<uqa_analysis::CompiledAnalyzer>,
        search_analyzer: Arc<uqa_analysis::CompiledAnalyzer>,
        rebuild: bool,
    ) -> Result<(), String> {
        let documents = if rebuild {
            Some(Self::project_fts_sources(table)?)
        } else {
            None
        };
        let mut index = table.inverted_index.write();
        if let Some(documents) = documents {
            index
                .rebuild_with_analyzer_revision(
                    field,
                    index_analyzer,
                    AnalyzerPhase::Index,
                    documents,
                )
                .map_err(|error| error.to_string())?;
        } else {
            index.set_field_analyzer_revision(field, index_analyzer, AnalyzerPhase::Index)?;
        }
        index.set_field_analyzer_revision(field, search_analyzer, AnalyzerPhase::Search)?;
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
            .and_then(FieldAnalyzerBinding::last_assignment))
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
    /// exact bound configuration JSON. `both` requires the same named revision on both sides; an unnamed default has no explicit assignment to return.
    pub fn get_table_analyzer(
        &self,
        table: &str,
        field: &str,
        phase: &str,
    ) -> Result<Option<String>, String> {
        self.synchronize_catalog_registries()
            .map_err(|error| error.to_string())?;
        let (_, phase) = normalize_analyzer_phase(phase)?;
        let Some(table) = self
            .try_resolve_table_name(table)
            .map_err(|error| error.to_string())?
        else {
            return Ok(None);
        };
        let Some(binding) = self
            .durable
            .table_field_analyzers
            .read()
            .get(&(table, field.to_owned()))
            .cloned()
        else {
            return Ok(None);
        };
        let side = match phase {
            AnalyzerPhase::Index => &binding.index,
            AnalyzerPhase::Search => &binding.search,
            AnalyzerPhase::Both
                if binding.index.name == binding.search.name
                    && binding.index.compiled.descriptor().fingerprint()
                        == binding.search.compiled.descriptor().fingerprint() =>
            {
                &binding.index
            }
            AnalyzerPhase::Both => return Ok(None),
        };
        if side.name.is_none() {
            return Ok(None);
        }
        let configuration = side
            .compiled
            .descriptor()
            .configuration()
            .map_err(|error| error.to_string())?;
        serde_json::to_string(&configuration)
            .map(Some)
            .map_err(|error| error.to_string())
    }
}
