//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{
    analyzer_registry, normalize_analyzer_phase, parse_analyzer_config, Analyzer, AnalyzerPhase,
    Engine,
};

impl Engine {
    pub(crate) fn resolve_analyzer(&self, name: &str) -> std::result::Result<Analyzer, String> {
        let name = name.trim();
        if name.is_empty() {
            return Err("analyzer name cannot be empty".into());
        }
        if let Ok(analyzer) = analyzer_registry::get_analyzer(name) {
            return Ok(analyzer);
        }
        let Some(config_json) = self.named_analyzers.read().get(name).cloned() else {
            return Err(format!("analyzer `{name}` is not registered"));
        };
        parse_analyzer_config(name, &config_json)
    }

    pub fn register_named_analyzer(
        &self,
        name: &str,
        config_json: &str,
    ) -> std::result::Result<(), String> {
        let _ = parse_analyzer_config(name, config_json)?;
        self.named_analyzers
            .write()
            .insert(name.to_string(), config_json.to_string());
        if let Some(catalog) = self.catalog.as_ref() {
            let _ = catalog.save_analyzer(name, config_json);
        }
        Ok(())
    }

    pub fn drop_named_analyzer(&self, name: &str) -> bool {
        let removed = self.named_analyzers.write().remove(name).is_some();
        if removed {
            if let Some(catalog) = self.catalog.as_ref() {
                let _ = catalog.drop_analyzer(name);
            }
        }
        removed
    }

    pub fn list_named_analyzers(&self) -> Vec<String> {
        let mut names: Vec<String> = self.named_analyzers.read().keys().cloned().collect();
        names.sort();
        names
    }

    pub fn set_table_field_analyzer(
        &self,
        table: &str,
        field: &str,
        analyzer_name: &str,
        phase: &str,
    ) -> std::result::Result<(), String> {
        let Some(t) = self.table(table) else {
            return Err(format!(
                "set_table_analyzer: table `{table}` does not exist"
            ));
        };
        let analyzer = self.resolve_analyzer(analyzer_name)?;
        let (phase_name, phase) = normalize_analyzer_phase(phase)?;
        t.inverted_index
            .write()
            .set_field_analyzer(field, analyzer, phase)
            .map_err(|e| format!("set_table_analyzer: {e}"))?;
        self.table_field_analyzers.write().insert(
            (table.to_string(), field.to_string()),
            (analyzer_name.to_string(), phase_name.clone()),
        );
        if let Some(catalog) = self.catalog.as_ref() {
            let _ = catalog.save_table_field_analyzer(table, field, &phase_name, analyzer_name);
        }
        if matches!(phase, AnalyzerPhase::Index | AnalyzerPhase::Both)
            && t.fts_fields().iter().any(|f| f == field)
        {
            Self::rebuild_fts_index(&t)?;
        }
        Ok(())
    }

    pub fn table_field_analyzer(&self, table: &str, field: &str) -> Option<(String, String)> {
        self.table_field_analyzers
            .read()
            .get(&(table.to_string(), field.to_string()))
            .cloned()
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
    pub fn drop_analyzer(&self, name: &str) -> bool {
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
    /// analyzer config JSON (the raw form the engine persists).
    /// Mirrors the canonical UQA implementation's `Engine.get_table_analyzer`.
    pub fn get_table_analyzer(&self, table: &str, field: &str, phase: &str) -> Option<String> {
        let (name, stored_phase) = self.table_field_analyzer(table, field)?;
        // Resolve the field's index/search analyzer based on the requested
        // phase; "both" means the override applies on both sides.
        let resolved = match (stored_phase.as_str(), phase) {
            ("both", _) | ("index", "index") | ("query" | "search", "search") => name,
            _ => return None,
        };
        self.resolve_analyzer(&resolved)
            .ok()
            .and_then(|analyzer| serde_json::to_string(&analyzer).ok())
    }
}
