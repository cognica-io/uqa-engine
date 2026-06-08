//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{
    build_histogram, build_mcv, default_runtime_parameter, distinct_count, Arc, BTreeMap,
    CatalogFacade, CatalogIndexRow, ColumnStatsInput, DocId, Engine, Ordering,
    StorageBackendResult, TableState, Value, VIEWS_METADATA_KEY,
};

impl Engine {
    pub fn register_view(&self, name: &str, body: uqa_sql::ast::SelectStmt) {
        let name = self.relation_name_for_create(name);
        self.views.write().insert(name.clone(), body);
        self.persist_views();
    }

    pub fn drop_view(&self, name: &str) -> bool {
        let Some(name) = self.resolve_view_name(name) else {
            return false;
        };
        let removed = self.views.write().remove(&name).is_some();
        if removed {
            self.persist_views();
        }
        removed
    }

    pub fn view(&self, name: &str) -> Option<uqa_sql::ast::SelectStmt> {
        let resolved = self.resolve_view_name(name)?;
        self.views.read().get(&resolved).cloned()
    }

    pub fn list_views(&self) -> Vec<String> {
        let mut out: Vec<String> = self.views.read().keys().cloned().collect();
        out.sort_unstable();
        out
    }

    fn persist_views(&self) {
        let Some(catalog) = self.catalog.as_ref() else {
            return;
        };
        if let Ok(json) = serde_json::to_string(&*self.views.read()) {
            let _ = catalog.set_metadata(VIEWS_METADATA_KEY, &json);
        }
    }

    pub(crate) fn restore_views_from_metadata(
        &self,
        catalog: &dyn CatalogFacade,
    ) -> StorageBackendResult<()> {
        let Some(json) = catalog.get_metadata(VIEWS_METADATA_KEY)? else {
            return Ok(());
        };
        if let Ok(views) = serde_json::from_str::<BTreeMap<String, uqa_sql::ast::SelectStmt>>(&json)
        {
            *self.views.write() = views;
        }
        Ok(())
    }

    pub fn list_catalog_indexes(&self) -> Vec<CatalogIndexRow> {
        let mut out: Vec<CatalogIndexRow> = self.catalog_indexes.read().values().cloned().collect();
        out.sort_by(|a, b| a.name.cmp(&b.name));
        out
    }

    /// Register a schema name. Schemas in the engine map onto
    /// optional table prefixes; the registry just records the name
    /// so subsequent statements that reference it do not error out.
    pub fn register_schema(&self, name: &str, _if_not_exists: bool) {
        self.schemas.write().insert(name.to_string());
    }

    pub fn drop_schema(&self, name: &str) -> bool {
        self.schemas.write().remove(name)
    }

    /// Sorted list of every registered schema. Mirrors the canonical UQA implementation's
    /// `Engine._tables.schemas`.
    pub fn list_schemas(&self) -> Vec<String> {
        let mut out: Vec<String> = self.schemas.read().iter().cloned().collect();
        if !out.iter().any(|s| s == "public") {
            out.insert(0, "public".to_string());
        }
        out
    }

    /// Tables that belong to a schema. Names matching `<schema>.X`
    /// are bucketed under `<schema>`; everything else falls under
    /// `public`. Mirrors the canonical UQA implementation's `Engine._tables.tables_in_schema`.
    pub fn tables_in_schema(&self, schema: &str) -> Vec<String> {
        let prefix = format!("{schema}.");
        let mut out: Vec<String> = Vec::new();
        for name in self.tables.read().keys() {
            if let Some(rest) = name.strip_prefix(&prefix) {
                out.push(rest.to_string());
            } else if schema == "public" && !name.contains('.') {
                out.push(name.clone());
            }
        }
        out.sort_unstable();
        out
    }

    pub fn list_sequences(&self) -> Vec<String> {
        let mut out: Vec<String> = self.sequences.read().keys().cloned().collect();
        out.sort_unstable();
        out
    }

    /// Current `search_path`. Mirrors the canonical UQA implementation's
    /// `Engine._tables.search_path`.
    pub fn search_path(&self) -> Vec<String> {
        self.search_path.read().clone()
    }

    /// Replace the `search_path`. Empty input falls back to `["public"]`.
    pub fn set_search_path(&self, path: Vec<String>) {
        let mut value = path;
        if value.is_empty() {
            value.push("public".to_string());
        }
        *self.search_path.write() = value;
    }

    /// Apply `SET <name> [TO|=] <value>`. Honours `search_path`
    /// directly; every other parameter is stored in the session-vars
    /// map so a subsequent `SHOW <name>` can echo it back. Mirrors
    /// the canonical UQA implementation's session-variable behaviour.
    pub fn set_variable(&self, name: &str, value: &str) {
        if name.eq_ignore_ascii_case("search_path") {
            let parts: Vec<String> = value
                .split(',')
                .map(|s| s.trim().trim_matches('"').trim_matches('\'').to_string())
                .filter(|s| !s.is_empty())
                .collect();
            self.set_search_path(parts);
            self.session_vars
                .write()
                .insert(name.to_string(), value.to_string());
            return;
        }
        self.session_vars
            .write()
            .insert(name.to_string(), value.to_string());
    }

    /// Read back a session variable. `search_path` always resolves to
    /// the current resolution order; every other key looks up the
    /// session-vars map, then PostgreSQL-compatible runtime defaults,
    /// and finally an empty string. Mirrors the canonical UQA
    /// implementation's `_compile_show`.
    pub fn show_variable(&self, name: &str) -> String {
        if name.eq_ignore_ascii_case("search_path") {
            return self.search_path().join(",");
        }
        let session_vars = self.session_vars.read();
        if let Some(value) = session_vars.get(name) {
            return value.clone();
        }
        if let Some((_, value)) = session_vars
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
        {
            return value.clone();
        }
        default_runtime_parameter(name).unwrap_or("").to_string()
    }

    /// Apply `DISCARD <target>`. Mirrors the canonical UQA implementation's `_compile_discard`:
    /// `ALL` resets every kind of session state; the narrower
    /// variants are scoped accordingly.
    pub fn discard(&self, target: uqa_sql::ast::DiscardTarget) {
        use uqa_sql::ast::DiscardTarget;
        match target {
            DiscardTarget::All => {
                self.session_vars.write().clear();
                self.prepared.write().clear();
                self.clear_sql_statement_cache();
                self.set_search_path(vec!["public".to_string()]);
                // Temp tables aren't tracked separately yet; clearing
                // the prepared map matches the canonical UQA implementation's effect on the bits
                // we own today.
            }
            DiscardTarget::Plans => {
                self.prepared.write().clear();
                self.clear_sql_statement_cache();
            }
            DiscardTarget::Sequences => {
                self.sequences.write().clear();
            }
            DiscardTarget::Temp => {
                // No temp-table registry yet; preserve the no-op
                // semantics until we add one.
            }
        }
    }

    /// Refresh per-column statistics for a single table or every
    /// table when `table` is `None`. Mirrors `Table.analyze` in
    /// the canonical UQA behavior: scans every document, collects per-
    /// column distinct count / null count / min / max / equi-depth
    /// histogram (100 buckets) / MCV list (top 10 above-average
    /// frequency), and stores the result on the per-table state so the
    /// cardinality estimator can read it on subsequent queries.
    pub fn run_analyze(&self, table: Option<&str>) {
        let names: Vec<String> = match table {
            Some(t) => vec![t.to_string()],
            None => self.tables.read().keys().cloned().collect(),
        };
        for name in names {
            let Some(t) = self.table(&name) else { continue };
            self.analyze_table(&name, &t);
        }
    }

    pub(crate) fn mark_column_stats_dirty(&self, table_name: &str, table: &Arc<TableState>) {
        if !table.column_stats_dirty.swap(true, Ordering::AcqRel) {
            if let Some(catalog) = self.catalog.as_ref() {
                let _ = catalog.delete_column_stats(table_name);
            }
        }
    }

    fn analyze_table(&self, table_name: &str, t: &Arc<TableState>) {
        let snapshot = t.document_store.read().snapshot();
        let doc_ids: Vec<DocId> = {
            let mut v = snapshot.doc_ids();
            v.sort_unstable();
            v
        };
        let n = doc_ids.len() as u64;
        let columns: Vec<String> = t.columns.read().iter().map(|c| c.name.clone()).collect();

        let mut col_values: BTreeMap<String, Vec<Value>> = BTreeMap::new();
        let mut col_nulls: BTreeMap<String, u64> = BTreeMap::new();
        for col in &columns {
            col_values.insert(col.clone(), Vec::new());
            col_nulls.insert(col.clone(), 0);
        }

        for doc_id in &doc_ids {
            let Some(doc) = snapshot.get(*doc_id) else {
                for col in &columns {
                    *col_nulls.get_mut(col).unwrap() += 1;
                }
                continue;
            };
            for col in &columns {
                match doc.get(col) {
                    None | Some(Value::Null) => {
                        *col_nulls.get_mut(col).unwrap() += 1;
                    }
                    Some(v) => {
                        col_values.get_mut(col).unwrap().push(v.clone());
                    }
                }
            }
        }

        let mut stats_out: BTreeMap<String, uqa_planner::ColumnStats> = BTreeMap::new();
        for col in &columns {
            let values = col_values.remove(col).unwrap_or_default();
            let null_count = col_nulls.remove(col).unwrap_or(0);
            let distinct = distinct_count(&values);
            let comparable: Vec<&Value> = values
                .iter()
                .filter(|v| {
                    matches!(
                        v,
                        Value::Int(_) | Value::Float(_) | Value::Str(_) | Value::Bool(_)
                    )
                })
                .collect();
            let min_val = comparable.iter().min().map(|v| (*v).clone());
            let max_val = comparable.iter().max().map(|v| (*v).clone());

            let histogram = build_histogram(&comparable);
            let (mcv_values, mcv_frequencies) = build_mcv(&values, n);

            stats_out.insert(
                col.clone(),
                uqa_planner::ColumnStats {
                    distinct_count: distinct,
                    null_count,
                    min_value: min_val,
                    max_value: max_val,
                    row_count: n,
                    histogram,
                    mcv_values,
                    mcv_frequencies,
                },
            );
        }

        if let Some(catalog) = self.catalog.as_ref() {
            let _ = Self::persist_column_stats(catalog.as_ref(), table_name, &stats_out);
        }
        *t.column_stats.write() = stats_out;
        t.column_stats_loaded.store(true, Ordering::Release);
        t.column_stats_dirty.store(false, Ordering::Release);
    }

    fn persist_column_stats(
        catalog: &dyn CatalogFacade,
        table_name: &str,
        stats: &BTreeMap<String, uqa_planner::ColumnStats>,
    ) -> StorageBackendResult<()> {
        catalog.delete_column_stats(table_name)?;
        for (col_name, cs) in stats {
            let min_json = cs
                .min_value
                .as_ref()
                .map(serde_json::to_string)
                .transpose()?;
            let max_json = cs
                .max_value
                .as_ref()
                .map(serde_json::to_string)
                .transpose()?;
            let histogram_json = serde_json::to_string(&cs.histogram)?;
            let mcv_values_json = serde_json::to_string(&cs.mcv_values)?;
            let mcv_frequencies_json = serde_json::to_string(&cs.mcv_frequencies)?;
            catalog.save_column_stats(ColumnStatsInput {
                table_name,
                column_name: col_name,
                distinct_count: Self::u64_to_i64(cs.distinct_count),
                null_count: Self::u64_to_i64(cs.null_count),
                min_value: min_json.as_deref(),
                max_value: max_json.as_deref(),
                row_count: Self::u64_to_i64(cs.row_count),
                histogram_json: &histogram_json,
                mcv_values_json: &mcv_values_json,
                mcv_frequencies_json: &mcv_frequencies_json,
            })?;
        }
        Ok(())
    }

    fn u64_to_i64(n: u64) -> i64 {
        i64::try_from(n).unwrap_or(i64::MAX)
    }

    /// Snapshot of the cardinality estimator's per-column statistics
    /// for `table`. Dirty stats are recomputed lazily so callers do not
    /// need to issue `ANALYZE` after every data change.
    pub fn column_stats(&self, table: &str) -> BTreeMap<String, uqa_planner::ColumnStats> {
        let Some(t) = self.table(table) else {
            return BTreeMap::new();
        };
        self.load_column_stats_if_needed(table, &t);
        if t.column_stats_dirty.load(Ordering::Acquire) {
            self.analyze_table(table, &t);
        }
        let stats = t.column_stats.read().clone();
        stats
    }

    fn load_column_stats_if_needed(&self, table: &str, t: &Arc<TableState>) {
        if t.column_stats_loaded.load(Ordering::Acquire) {
            return;
        }
        if t.column_stats_loaded
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return;
        }
        let stats = self
            .catalog
            .as_ref()
            .and_then(|catalog| Self::load_column_stats_from_catalog(catalog.as_ref(), table).ok())
            .unwrap_or_default();
        let missing_stats = stats.is_empty() && !t.columns.read().is_empty();
        *t.column_stats.write() = stats;
        if missing_stats {
            t.column_stats_dirty.store(true, Ordering::Release);
        }
    }
}
