//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{
    AnalyzerPhase, Arc, BTreeMap, DocId, Document, Engine, FieldName, IVFIndexParams, SQLError,
    StorageBackendError, StorageBackendResult, TableState, Value,
};
use crate::CatalogIndexRow;

/// Answer of the value-index conflict probe in [`Engine::find_conflict`].
enum IndexConflictProbe {
    /// No conflict column has a usable value index; fall back to the
    /// evaluated document scan.
    Unanswerable,
    /// The index answered: no existing row matches the conflict target.
    NoConflict,
    /// The index answered: this existing row matches the conflict target.
    Conflict(DocId),
}

impl Engine {
    fn catalog_index_columns(row: &CatalogIndexRow) -> Vec<String> {
        serde_json::from_str(&row.columns_json).unwrap_or_default()
    }

    fn catalog_index_references_column(row: &CatalogIndexRow, column: &str) -> bool {
        Self::catalog_index_columns(row)
            .iter()
            .any(|candidate| candidate.eq_ignore_ascii_case(column))
    }

    fn catalog_index_with_renamed_column(
        mut row: CatalogIndexRow,
        from: &str,
        to: &str,
    ) -> CatalogIndexRow {
        let mut columns = Self::catalog_index_columns(&row);
        let mut changed = false;
        for column in &mut columns {
            if column.eq_ignore_ascii_case(from) {
                *column = to.to_string();
                changed = true;
            }
        }
        if changed {
            row.columns_json = serde_json::to_string(&columns).unwrap_or(row.columns_json);
        }
        row
    }

    fn remove_catalog_indexes_for_column(&self, table: &str, column: &str) {
        self.catalog_indexes.write().retain(|_, row| {
            !(row.table_name == table && Self::catalog_index_references_column(row, column))
        });
    }

    fn rename_catalog_index_table_refs(&self, from: &str, to: &str) {
        for row in self.catalog_indexes.write().values_mut() {
            if row.table_name == from {
                row.table_name = to.to_string();
            }
        }
    }

    fn rename_catalog_index_column_refs(&self, table: &str, from: &str, to: &str) {
        let mut rows = self.catalog_indexes.write();
        for row in rows.values_mut() {
            if row.table_name == table && Self::catalog_index_references_column(row, from) {
                let renamed = Self::catalog_index_with_renamed_column(row.clone(), from, to);
                row.columns_json = renamed.columns_json;
            }
        }
    }

    fn ivf_catalog_params_for_column(&self, table: &str, column: &str) -> Option<IVFIndexParams> {
        self.catalog_indexes.read().values().find_map(|row| {
            let is_vector_index = row.index_type.eq_ignore_ascii_case("ivf")
                || row.index_type.eq_ignore_ascii_case("hnsw");
            (row.table_name == table
                && is_vector_index
                && Self::catalog_index_references_column(row, column))
            .then(|| {
                let parameters: BTreeMap<String, String> =
                    serde_json::from_str(&row.parameters_json).unwrap_or_default();
                IVFIndexParams::from_map_lossy(&parameters)
            })
        })
    }

    fn vector_catalog_index_names_for_column(&self, table: &str, column: &str) -> Vec<String> {
        self.catalog_indexes
            .read()
            .values()
            .filter(|row| {
                row.table_name == table
                    && (row.index_type.eq_ignore_ascii_case("ivf")
                        || row.index_type.eq_ignore_ascii_case("hnsw"))
                    && Self::catalog_index_references_column(row, column)
            })
            .map(|row| row.name.clone())
            .collect()
    }

    fn rebind_persistent_table_stores(&self, table_name: &str, table: &TableState) {
        let Some(backend) = self.backend.as_ref() else {
            return;
        };
        let analyzer = table.analyzer.read().clone();
        *table.document_store.write() = backend.document_store(table_name);
        table
            .doc_count_dirty
            .store(true, std::sync::atomic::Ordering::Release);
        Self::value_indexes_clear(table);
        *table.inverted_index.write() = backend.inverted_index(table_name, analyzer);

        let analyzer_rows: Vec<(String, String, String)> = self
            .table_field_analyzers
            .read()
            .iter()
            .filter(|((table, _), _)| table == table_name)
            .map(|((_, field), (analyzer, phase))| (field.clone(), analyzer.clone(), phase.clone()))
            .collect();
        for (field, analyzer_name, phase) in analyzer_rows {
            if let Ok(analyzer) = self.resolve_analyzer(&analyzer_name) {
                let phase = if phase.eq_ignore_ascii_case("index") {
                    AnalyzerPhase::Index
                } else if phase.eq_ignore_ascii_case("search") {
                    AnalyzerPhase::Search
                } else {
                    AnalyzerPhase::Both
                };
                let _ = table
                    .inverted_index
                    .write()
                    .set_field_analyzer(&field, analyzer, phase);
            }
        }

        let vector_fields: Vec<(String, u32)> = table
            .vector_indexes
            .read()
            .iter()
            .map(|(field, idx)| (field.clone(), idx.dimensions()))
            .collect();
        let mut rebound = BTreeMap::new();
        for (field, dimensions) in vector_fields {
            let idx = if let Some(params) = self.ivf_catalog_params_for_column(table_name, &field) {
                self.build_vector_index_for_restore(table_name, &field, dimensions, params)
            } else {
                self.build_vector_index_with_initialize(table_name, &field, dimensions, None, false)
            };
            rebound.insert(field, idx);
        }
        *table.vector_indexes.write() = rebound;
    }

    pub(crate) fn document_vector_values(
        table: &Arc<TableState>,
        document: &Document,
    ) -> BTreeMap<FieldName, Vec<Vec<f32>>> {
        let vector_fields: Vec<FieldName> = table.vector_indexes.read().keys().cloned().collect();
        let mut vectors = BTreeMap::new();
        for field in vector_fields {
            let Some(value) = document.get(&field) else {
                continue;
            };
            if let Some(values) = Self::field_index_vectors(table, &field, value) {
                vectors.insert(field, values);
            }
        }
        vectors
    }

    pub(crate) fn field_index_vectors(
        table: &TableState,
        field: &str,
        value: &Value,
    ) -> Option<Vec<Vec<f32>>> {
        let ty = table
            .columns
            .read()
            .iter()
            .find(|column| column.name == field)
            .map(|column| column.ty.clone());
        match ty {
            Some(uqa_sql::ast::ColumnType::Tensor(dim)) => {
                let tensor = uqa_sql::expr::value_to_tensor(value).ok()?;
                tensor
                    .iter()
                    .all(|vector| vector.len() as u32 == dim)
                    .then_some(tensor)
            }
            Some(uqa_sql::ast::ColumnType::Vector(dim)) => {
                let vector = uqa_sql::expr::value_to_vector(value).ok()?;
                (vector.len() as u32 == dim).then_some(vec![vector])
            }
            _ => uqa_sql::expr::value_to_vector(value)
                .ok()
                .map(|vector| vec![vector]),
        }
    }

    /// Drop a table from the catalog and release its in-memory state.
    /// Returns `true` if the table existed.
    pub fn drop_table(&self, name: &str) -> bool {
        self.try_drop_table(name).unwrap_or(false)
    }

    pub(crate) fn try_drop_table(&self, name: &str) -> StorageBackendResult<bool> {
        let Some(name) = self.resolve_table_name(name) else {
            return Ok(false);
        };
        if !self.tables.read().contains_key(&name) {
            return Ok(false);
        }
        if let Some(catalog) = self.catalog.as_ref() {
            catalog.drop_table(&name)?;
            catalog.purge_table_data(&name)?;
            catalog.drop_table_field_analyzers(&name)?;
            catalog.drop_catalog_indexes_for_table(&name)?;
        }
        self.tables.write().remove(&name);
        // Sweep every related per-table registry so catalog state
        // does not outlive the table.
        self.table_field_analyzers
            .write()
            .retain(|(t, _), _| t != &name);
        self.catalog_indexes
            .write()
            .retain(|_, row| row.table_name != name);
        Ok(true)
    }

    pub fn has_table(&self, name: &str) -> bool {
        self.resolve_table_name(name).is_some()
    }

    /// All schema-declared columns for `table`, in declaration order.
    pub fn table_columns(&self, table: &str) -> Vec<String> {
        self.table(table)
            .map(|t| t.columns.read().iter().map(|c| c.name.clone()).collect())
            .unwrap_or_default()
    }

    pub fn table_has_column(&self, table: &str, column: &str) -> bool {
        let Some(t) = self.table(table) else {
            return false;
        };
        let cols = t.columns.read();
        cols.iter().any(|c| c.name == column)
    }

    pub(crate) fn column_type(
        &self,
        table: &str,
        column: &str,
    ) -> Option<uqa_sql::ast::ColumnType> {
        let t = self.table(table)?;
        let cols = t.columns.read();
        cols.iter().find(|c| c.name == column).map(|c| c.ty.clone())
    }

    /// Return the SERIAL/BIGSERIAL column name for `table`, if any.
    pub(crate) fn auto_increment_column(&self, table: &str) -> Option<String> {
        let t = self.table(table)?;
        let cols = t.columns.read();
        cols.iter()
            .find(|c| c.auto_increment)
            .map(|c| c.name.clone())
    }

    /// Sorted list of every registered table name.
    pub fn table_names(&self) -> Vec<String> {
        self.tables.read().keys().cloned().collect()
    }

    /// Snapshot the column schema of `table`. Returns `None` when no
    /// table by that name is registered.
    pub fn describe_table(&self, table: &str) -> Option<Vec<uqa_sql::ast::ColumnDef>> {
        self.table(table).map(|t| t.columns.read().clone())
    }

    /// DEFAULT expression for `column` on `table`, when one was
    /// declared via `... <col> <type> DEFAULT <expr>`.
    pub fn column_default_expr(&self, table: &str, column: &str) -> Option<uqa_sql::ast::Expr> {
        let t = self.table(table)?;
        let cols = t.columns.read();
        cols.iter()
            .find(|c| c.name == column)
            .and_then(|c| c.default.clone())
    }

    pub fn set_column_default(
        &self,
        table: &str,
        column: &str,
        default: Option<uqa_sql::ast::Expr>,
    ) -> bool {
        let Some(table_name) = self.resolve_table_name(table) else {
            return false;
        };
        let Some(t) = self.table(table) else {
            return false;
        };
        let mut found = false;
        {
            let mut cols = t.columns.write();
            if let Some(col) = cols.iter_mut().find(|col| col.name == column) {
                col.default = default;
                found = true;
            }
        }
        if found && self.is_persistent() {
            self.save_table_schema(&table_name, &t);
        }
        if found {
            self.mark_column_stats_dirty(&table_name, &t);
        }
        found
    }

    pub fn set_column_not_null(&self, table: &str, column: &str, not_null: bool) -> bool {
        let Some(table_name) = self.resolve_table_name(table) else {
            return false;
        };
        let Some(t) = self.table(table) else {
            return false;
        };
        let mut found = false;
        {
            let mut cols = t.columns.write();
            for col in cols.iter_mut() {
                if col.name == column {
                    col.not_null = not_null;
                    found = true;
                    break;
                }
            }
        }
        if found && self.is_persistent() {
            self.save_table_schema(&table_name, &t);
        }
        if found {
            self.mark_column_stats_dirty(&table_name, &t);
        }
        found
    }

    pub fn set_column_type(
        &self,
        table: &str,
        column: &str,
        ty: &uqa_sql::ast::ColumnType,
    ) -> bool {
        let Some(table_name) = self.resolve_table_name(table) else {
            return false;
        };
        let Some(t) = self.table(table) else {
            return false;
        };
        let mut found = false;
        {
            let mut cols = t.columns.write();
            if let Some(col) = cols.iter_mut().find(|col| col.name == column) {
                col.ty.clone_from(ty);
                found = true;
            }
        }
        if found && self.is_persistent() {
            self.save_table_schema(&table_name, &t);
        }
        if found {
            self.mark_column_stats_dirty(&table_name, &t);
        }
        found
    }

    /// Register table-level CHECK + FK constraints. Called by the
    /// SQL `CREATE TABLE` path after the columns are in place.
    pub fn register_table_constraints(
        &self,
        table: &str,
        checks: Vec<uqa_sql::ast::TableCheck>,
        foreign_keys: Vec<uqa_sql::ast::ForeignKey>,
    ) {
        let Some(t) = self.table(table) else { return };
        *t.table_checks.write() = checks;
        *t.foreign_keys.write() = foreign_keys;
    }

    /// Snapshot of every CHECK constraint that applies to `table`,
    /// merging the column-level CHECKs into the table-level list.
    /// Returns `(name, expr)` pairs where `name` is the constraint
    /// name when one was supplied (synthesised as `<col>_check` for
    /// column-level constraints).
    pub fn check_constraints(&self, table: &str) -> Vec<(Option<String>, uqa_sql::ast::Expr)> {
        let Some(t) = self.table(table) else {
            return Vec::new();
        };
        let mut out: Vec<(Option<String>, uqa_sql::ast::Expr)> = Vec::new();
        for col in t.columns.read().iter() {
            if let Some(expr) = col.check.clone() {
                out.push((Some(format!("{}_check", col.name)), expr));
            }
        }
        for c in t.table_checks.read().iter() {
            out.push((c.name.clone(), c.expr.clone()));
        }
        out
    }

    /// Snapshot of every FOREIGN KEY constraint that applies to
    /// `table`. Column-level `REFERENCES` are lifted to single-column
    /// `ForeignKey` entries.
    pub fn foreign_keys(&self, table: &str) -> Vec<uqa_sql::ast::ForeignKey> {
        let Some(t) = self.table(table) else {
            return Vec::new();
        };
        let mut out: Vec<uqa_sql::ast::ForeignKey> = t.foreign_keys.read().clone();
        for col in t.columns.read().iter() {
            if let Some(reference) = col.references.clone() {
                out.push(uqa_sql::ast::ForeignKey {
                    name: Some(format!("{}_fkey", col.name)),
                    local_columns: vec![col.name.clone()],
                    ref_table: reference.table,
                    ref_columns: vec![reference.column],
                    on_update: reference.on_update,
                    on_delete: reference.on_delete,
                    on_delete_set_columns: Vec::new(),
                    match_type: reference.match_type,
                });
            }
        }
        out
    }

    /// Tables that hold a FOREIGN KEY pointing at `table`. Used by
    /// DELETE / DROP CASCADE to refuse the operation when a referrer
    /// has at least one row matching the target value.
    pub fn referrers_to(&self, table: &str) -> Vec<(String, uqa_sql::ast::ForeignKey)> {
        let mut out: Vec<(String, uqa_sql::ast::ForeignKey)> = Vec::new();
        let names: Vec<String> = self.tables.read().keys().cloned().collect();
        for other in names {
            if other == table {
                continue;
            }
            for fk in self.foreign_keys(&other) {
                if fk.ref_table == table {
                    out.push((other.clone(), fk));
                }
            }
        }
        out
    }

    /// Names of columns with a `UNIQUE` or `PRIMARY KEY` constraint
    /// declared on the table. Auto-increment columns are excluded
    /// because the engine guarantees their uniqueness through the
    /// monotonic id watermark, so re-checking is redundant.
    pub fn unique_columns(&self, table: &str) -> Vec<String> {
        let Some(t) = self.table(table) else {
            return Vec::new();
        };
        let cols = t.columns.read();
        cols.iter()
            .filter(|c| (c.unique || c.primary_key) && !c.auto_increment)
            .map(|c| c.name.clone())
            .collect()
    }

    /// Allocate the next id from the per-table watermark, returning the
    /// allocated value. Updates the watermark in place.
    pub(crate) fn allocate_next_id(&self, table: &str) -> Result<u64, SQLError> {
        let t = self
            .table(table)
            .ok_or_else(|| SQLError::Internal(format!("unknown table `{table}`")))?;
        let mut g = t.next_id.lock();
        let id = *g;
        *g = id.saturating_add(1);
        Ok(id)
    }

    /// Move the watermark past `doc_id` if needed (called after a manual
    /// id assignment so the next allocation does not collide).
    pub(crate) fn advance_next_id(&self, table: &str, doc_id: DocId) {
        let Some(t) = self.table(table) else {
            return;
        };
        let mut g = t.next_id.lock();
        if doc_id >= *g {
            *g = doc_id + 1;
        }
    }

    /// Append a column to the schema. No data migration is needed because
    /// the document store is sparse; rows missing the column read back as
    /// `Value::Null`.
    pub fn register_column(&self, table: &str, column: uqa_sql::ast::ColumnDef) {
        let _ = self.try_register_column(table, column);
    }

    pub(crate) fn try_register_column(
        &self,
        table: &str,
        column: uqa_sql::ast::ColumnDef,
    ) -> StorageBackendResult<()> {
        let Some(table_name) = self.resolve_table_name(table) else {
            return Ok(());
        };
        let Some(t) = self.table(table) else {
            return Ok(());
        };
        if t.columns.read().iter().any(|c| c.name == column.name) {
            return Ok(());
        }
        t.columns.write().push(column);
        self.mark_column_stats_dirty(&table_name, &t);
        if self.is_persistent() {
            self.try_save_table_schema(&table_name, &t)?;
        }
        self.refresh_value_indexes_for_table(&table_name)?;
        Ok(())
    }

    pub fn drop_column(&self, table: &str, column: &str) {
        let _ = self.try_drop_column(table, column);
    }

    pub(crate) fn try_drop_column(&self, table: &str, column: &str) -> StorageBackendResult<bool> {
        let Some(table_name) = self.resolve_table_name(table) else {
            return Ok(false);
        };
        let Some(t) = self.table(table) else {
            return Ok(false);
        };
        Self::value_indexes_clear(&t);
        {
            let mut cols = t.columns.write();
            cols.retain(|c| c.name != column);
        }
        // Remove from FTS field list if present.
        {
            let mut fts = t.fts_fields.write();
            fts.retain(|f| f != column);
        }
        // Drop the vector index for this field if it exists.
        {
            let mut vs = t.vector_indexes.write();
            if let Some(mut idx) = vs.remove(column) {
                idx.clear();
            }
        }
        self.remove_catalog_indexes_for_column(&table_name, column);
        self.table_field_analyzers
            .write()
            .retain(|(table, field), _| !(table == &table_name && field == column));
        let ids: Vec<DocId> = t.document_store.read().snapshot().doc_ids();
        for doc_id in ids {
            let Some(mut doc) = t.document_store.read().get(doc_id) else {
                continue;
            };
            if doc.remove(column).is_some() {
                self.rewrite_document_for_schema_change(&table_name, doc_id, doc)
                    .map_err(|err| StorageBackendError::Other(err.to_string()))?;
            }
        }
        if self.is_persistent() {
            if let Some(catalog) = self.catalog.as_ref() {
                catalog.drop_column_data(&table_name, column)?;
            }
            self.try_save_table_schema(&table_name, &t)?;
        }
        self.mark_column_stats_dirty(&table_name, &t);
        self.refresh_value_indexes_for_table(&table_name)?;
        Ok(true)
    }

    pub(crate) fn try_drop_vector_indexes_for_column(
        &self,
        table: &str,
        column: &str,
    ) -> StorageBackendResult<bool> {
        let Some(table_name) = self.resolve_table_name(table) else {
            return Ok(false);
        };
        let Some(t) = self.table(table) else {
            return Ok(false);
        };
        if let Some(mut idx) = t.vector_indexes.write().remove(column) {
            idx.clear();
        }
        for index_name in self.vector_catalog_index_names_for_column(&table_name, column) {
            self.try_drop_catalog_index(&index_name)?;
        }
        self.try_save_table_schema(&table_name, &t)?;
        Ok(true)
    }

    pub(crate) fn try_rebuild_vector_index_for_column(
        &self,
        table: &str,
        column: &str,
        dimensions: u32,
    ) -> StorageBackendResult<bool> {
        let Some(table_name) = self.resolve_table_name(table) else {
            return Ok(false);
        };
        let params = self.ivf_catalog_params_for_column(&table_name, column);
        let rebuilt = if let Some(params) = params {
            self.rebuild_ivf_vector_field(&table_name, column, dimensions, params)
        } else {
            self.rebuild_vector_field(&table_name, column, dimensions)
        };
        if !rebuilt {
            return Err(StorageBackendError::Other(format!(
                "failed to rebuild vector index for `{table_name}`.`{column}`"
            )));
        }
        if let Some(t) = self.table(&table_name) {
            self.try_save_table_schema(&table_name, &t)?;
        }
        Ok(true)
    }

    pub fn rename_column(&self, table: &str, from: &str, to: &str) {
        let _ = self.try_rename_column(table, from, to);
    }

    pub(crate) fn try_rename_column(
        &self,
        table: &str,
        from: &str,
        to: &str,
    ) -> StorageBackendResult<bool> {
        let Some(table_name) = self.resolve_table_name(table) else {
            return Ok(false);
        };
        let Some(t) = self.table(table) else {
            return Ok(false);
        };
        Self::value_indexes_clear(&t);
        {
            let mut cols = t.columns.write();
            for c in cols.iter_mut() {
                if c.name == from {
                    c.name = to.to_string();
                }
            }
        }
        {
            let mut fts = t.fts_fields.write();
            for f in fts.iter_mut() {
                if f == from {
                    *f = to.to_string();
                }
            }
        }
        let vector_dimensions = {
            let mut vs = t.vector_indexes.write();
            vs.remove(from).map(|mut idx| {
                let dimensions = idx.dimensions();
                idx.clear();
                dimensions
            })
        };
        let ids: Vec<DocId> = t.document_store.read().snapshot().doc_ids();
        for doc_id in ids {
            let Some(mut doc) = t.document_store.read().get(doc_id) else {
                continue;
            };
            if let Some(value) = doc.remove(from) {
                doc.insert(to.to_string(), value);
                self.rewrite_document_for_schema_change(&table_name, doc_id, doc)
                    .map_err(|err| StorageBackendError::Other(err.to_string()))?;
            }
        }
        if let Some(dimensions) = vector_dimensions {
            self.create_vector_field(&table_name, to, dimensions);
        }
        self.rename_catalog_index_column_refs(&table_name, from, to);
        {
            let mut analyzers = self.table_field_analyzers.write();
            let mut moved = Vec::new();
            analyzers.retain(|(table, field), value| {
                if table == &table_name && field == from {
                    moved.push(((table_name.clone(), to.to_string()), value.clone()));
                    false
                } else {
                    true
                }
            });
            analyzers.extend(moved);
        }
        if self.is_persistent() {
            if let Some(catalog) = self.catalog.as_ref() {
                catalog.rename_column_data(&table_name, from, to)?;
            }
            if let Some(dimensions) = vector_dimensions {
                if let Some(params) = self.ivf_catalog_params_for_column(&table_name, to) {
                    if !self.rebuild_ivf_vector_field(&table_name, to, dimensions, params) {
                        return Err(StorageBackendError::Other(format!(
                            "failed to rebuild IVF index for `{table_name}`.`{to}`"
                        )));
                    }
                }
            }
            self.try_save_table_schema(&table_name, &t)?;
        }
        self.mark_column_stats_dirty(&table_name, &t);
        self.refresh_value_indexes_for_table(&table_name)?;
        Ok(true)
    }

    pub fn rename_table(&self, from: &str, to: &str) -> bool {
        self.try_rename_table(from, to).unwrap_or(false)
    }

    pub(crate) fn try_rename_table(&self, from: &str, to: &str) -> StorageBackendResult<bool> {
        let Some(from) = self.resolve_table_name(from) else {
            return Ok(false);
        };
        let to = self.relation_name_for_create(to);
        {
            let tables = self.tables.read();
            if !tables.contains_key(&from) || tables.contains_key(&to) {
                return Ok(false);
            }
        }
        if self.is_persistent() {
            if let Some(catalog) = self.catalog.as_ref() {
                catalog.rename_table_data(&from, &to)?;
            }
        }
        let mut tables = self.tables.write();
        if tables.contains_key(&to) {
            return Ok(false);
        }
        let Some(state) = tables.remove(&from) else {
            return Ok(false);
        };
        tables.insert(to.clone(), state.clone());
        drop(tables);
        self.rename_catalog_index_table_refs(&from, &to);
        {
            let mut analyzers = self.table_field_analyzers.write();
            let mut moved = Vec::new();
            analyzers.retain(|(table, field), value| {
                if table == &from {
                    moved.push(((to.clone(), field.clone()), value.clone()));
                    false
                } else {
                    true
                }
            });
            analyzers.extend(moved);
        }
        if self.is_persistent() {
            self.rebind_persistent_table_stores(&to, &state);
            self.try_save_table_schema(&to, &state)?;
        }
        self.mark_column_stats_dirty(&to, &state);
        self.refresh_value_indexes_for_table(&to)?;
        Ok(true)
    }

    /// Append `field` to the table's FTS field list. Existing rows are
    /// indexed immediately so SQL `CREATE INDEX USING gin` behaves like a
    /// real secondary-index build rather than a metadata-only toggle.
    pub fn add_fts_field(&self, table: &str, field: FieldName) -> Result<(), String> {
        self.add_fts_field_with_analyzer(table, field, None)
    }

    /// Same as [`Engine::add_fts_field`], but allows registering a
    /// per-field analyzer name (e.g. `standard_cjk`). When `None`, the
    /// table-level analyzer continues to apply.
    pub fn add_fts_field_with_analyzer(
        &self,
        table: &str,
        field: FieldName,
        analyzer: Option<&str>,
    ) -> Result<(), String> {
        let table_name = self
            .resolve_table_name(table)
            .ok_or_else(|| format!("unknown table `{table}`"))?;
        let t = self
            .table(table)
            .ok_or_else(|| format!("unknown table `{table}`"))?;
        if let Some(analyzer_name) = analyzer {
            let analyzer = self.resolve_analyzer(analyzer_name)?;
            t.inverted_index
                .write()
                .set_field_analyzer(&field, analyzer, AnalyzerPhase::Both)
                .map_err(|e| format!("add_fts_field: {e}"))?;
            self.table_field_analyzers.write().insert(
                (table_name.clone(), field.clone()),
                (analyzer_name.to_string(), "both".to_string()),
            );
            if let Some(catalog) = self.catalog.as_ref() {
                let _ =
                    catalog.save_table_field_analyzer(&table_name, &field, "both", analyzer_name);
            }
        }
        {
            let mut fts = t.fts_fields.write();
            if !fts.contains(&field) {
                fts.push(field);
            }
        }
        Self::rebuild_fts_index(&t)?;
        if self.is_persistent() {
            self.save_table_schema(&table_name, &t);
        }
        Ok(())
    }

    pub fn get_document(&self, table: &str, doc_id: DocId) -> Option<Document> {
        let t = self.table(table)?;
        let got = t.document_store.read().get(doc_id);
        got
    }

    /// Fetch many documents in one storage round trip. Ids without a
    /// stored document are absent from the result. Persistent backends
    /// batch the reads; the memory backend falls back to per-id gets.
    pub(crate) fn get_documents_bulk(
        &self,
        table: &str,
        doc_ids: &[DocId],
    ) -> BTreeMap<DocId, Document> {
        let Some(t) = self.table(table) else {
            return BTreeMap::new();
        };
        let got = t.document_store.read().get_many(doc_ids);
        got
    }

    /// Fetch a column projection for many documents in one round trip.
    /// The value vector aligns with `fields`; missing fields are Null.
    /// Persistent backends extract the fields inside the storage scan
    /// so whole documents never materialise.
    pub(crate) fn get_document_fields_multi(
        &self,
        table: &str,
        doc_ids: &[DocId],
        fields: &[&str],
    ) -> BTreeMap<DocId, Vec<Value>> {
        let Some(t) = self.table(table) else {
            return BTreeMap::new();
        };
        let got = t.document_store.read().get_fields_multi(doc_ids, fields);
        got
    }

    pub(crate) fn for_each_document_fields_multi_ref(
        &self,
        table: &str,
        doc_ids: &[DocId],
        fields: &[&str],
        visitor: &mut dyn FnMut(DocId, &[&Value]) -> bool,
    ) {
        let Some(t) = self.table(table) else {
            return;
        };
        t.document_store
            .read()
            .for_each_fields_multi_ref(doc_ids, fields, visitor);
    }

    pub(crate) fn get_document_fields(
        &self,
        table: &str,
        doc_ids: &[DocId],
        field: &str,
    ) -> BTreeMap<DocId, Value> {
        let Some(t) = self.table(table) else {
            return BTreeMap::new();
        };
        let values = t.document_store.read().get_fields_bulk(doc_ids, field);
        values
    }

    pub fn find_doc_id_by_field(&self, table: &str, field: &str, value: &Value) -> Option<DocId> {
        let t = self.table(table)?;
        let found = t.document_store.read().find_doc_id_by_field(field, value);
        found
    }

    /// Find the first document whose conflict columns all match the
    /// given values. Returns the existing doc id when a conflict
    /// exists, `None` when the row would be a fresh insert. Mirrors
    /// `PostgreSQL`'s `ON CONFLICT (col, ...)` lookup; the conflict
    /// columns map to the unique-constraint target.
    ///
    /// Lookup order: the integer-primary-key slot mapping, then a
    /// value-index equality probe on the first index-answerable
    /// conflict column (conflict targets are PRIMARY KEY / UNIQUE
    /// columns, exactly the columns [`Self::value_indexable_fields`]
    /// admits), and only then the evaluated document scan. The index
    /// probe is what keeps per-row UNIQUE and FOREIGN KEY validation
    /// `O(log n)` during bulk inserts -- previously every insert into a
    /// table with a non-integer unique column re-scanned all documents,
    /// making an n-row load `O(n^2)`.
    pub fn find_conflict(
        &self,
        table: &str,
        conflict_columns: &[String],
        values: &[Value],
    ) -> Option<DocId> {
        if conflict_columns.is_empty() || conflict_columns.len() != values.len() {
            return None;
        }
        let t = self.table(table)?;
        if conflict_columns.len() == 1 {
            if let Some(doc_id) =
                Self::doc_id_for_primary_key_conflict(&t, &conflict_columns[0], &values[0])
            {
                if doc_id >= *t.next_id.lock() {
                    return None;
                }
                return t
                    .document_store
                    .read()
                    .contains_doc_id(doc_id)
                    .then_some(doc_id);
            }
        }
        match self.find_conflict_via_value_index(&t, table, conflict_columns, values) {
            IndexConflictProbe::Conflict(doc_id) => return Some(doc_id),
            IndexConflictProbe::NoConflict => return None,
            IndexConflictProbe::Unanswerable => {}
        }
        let found = t
            .document_store
            .read()
            .find_doc_id_by_fields(conflict_columns, values);
        found
    }

    /// Index-backed conflict lookup. `Unanswerable` means no conflict
    /// column could be answered by a value index (unindexed columns, or
    /// the temporal/NaN semantics guard refused) and the caller must
    /// fall back to the evaluated scan. Otherwise the answer is
    /// authoritative: candidates narrow through the pivot column's
    /// posting list in `O(log n + k)` and the remaining columns verify
    /// against stored fields on those candidates only, with the same
    /// `Value` equality the evaluated scan uses. An empty posting list
    /// is an authoritative `NoConflict`, which is the common case on
    /// insert and must not degrade into a scan.
    fn find_conflict_via_value_index(
        &self,
        t: &TableState,
        table: &str,
        conflict_columns: &[String],
        values: &[Value],
    ) -> IndexConflictProbe {
        for (pivot, (column, value)) in conflict_columns.iter().zip(values.iter()).enumerate() {
            let Some(candidates) =
                self.value_index_scan(table, column, &uqa_core::Predicate::Equals(value.clone()))
            else {
                continue;
            };
            let store = t.document_store.read();
            let found = candidates
                .entries()
                .iter()
                .map(|entry| entry.doc_id)
                .find(|doc_id| {
                    conflict_columns.iter().zip(values.iter()).enumerate().all(
                        |(index, (col, val))| {
                            index == pivot
                                || store.get_field(*doc_id, col).unwrap_or(Value::Null) == *val
                        },
                    )
                });
            return match found {
                Some(doc_id) => IndexConflictProbe::Conflict(doc_id),
                None => IndexConflictProbe::NoConflict,
            };
        }
        IndexConflictProbe::Unanswerable
    }

    fn doc_id_for_primary_key_conflict(
        table: &TableState,
        column: &str,
        value: &Value,
    ) -> Option<DocId> {
        let Value::Int(id) = value else {
            return None;
        };
        if *id < 0 {
            return None;
        }
        let columns = table.columns.read();
        let maps_to_doc_id = columns.iter().any(|col| {
            col.name == column
                && col.primary_key
                && matches!(col.ty, uqa_sql::ast::ColumnType::Integer)
        });
        if !maps_to_doc_id {
            return None;
        }
        Some(*id as DocId)
    }

    /// Apply per-column updates to an existing document. Mirrors the
    /// `DO UPDATE SET col = expr` branch of an ON CONFLICT clause.
    /// Returns whether the row was updated; `Ok(false)` when the
    /// document no longer exists. Storage write failures surface as
    /// `Err` so the enclosing transaction rolls back instead of
    /// committing a delete whose re-insert never happened.
    pub fn update_document_fields(
        &self,
        table: &str,
        doc_id: DocId,
        updates: BTreeMap<String, Value>,
        vectors: BTreeMap<String, Vec<f32>>,
    ) -> Result<bool, SQLError> {
        let vector_values = vectors
            .into_iter()
            .map(|(field, vector)| (field, vec![vector]))
            .collect();
        self.update_document_fields_with_vector_values(table, doc_id, updates, vector_values)
    }

    pub fn update_document_fields_with_vector_values(
        &self,
        table: &str,
        doc_id: DocId,
        updates: BTreeMap<String, Value>,
        vectors: BTreeMap<String, Vec<Vec<f32>>>,
    ) -> Result<bool, SQLError> {
        let Some(t) = self.table(table) else {
            return Ok(false);
        };
        let Some(mut doc) = t.document_store.read().get(doc_id) else {
            return Ok(false);
        };
        for (k, v) in updates {
            doc.insert(k, v);
        }
        // Re-add the document so the inverted index picks up the new
        // text fields. Unindex the old field values first; the re-add
        // path indexes the new ones. The delete must not proceed to
        // index surgery when it fails, and a failed re-add must abort
        // the statement.
        let old_indexed = Self::value_indexes_old_values(&t, doc_id);
        let mut store = t.document_store.write();
        store
            .delete(doc_id)
            .map_err(|err| document_store_write_error(&err))?;
        self.persist_value_indexes_apply_write(table, doc_id, None)?;
        if let Some(old) = old_indexed.as_ref() {
            Self::value_indexes_apply_write(&t, doc_id, Some(old), None);
        }
        drop(store);
        t.inverted_index.write().remove_document(doc_id);
        for idx in t.vector_indexes.write().values_mut() {
            idx.as_mut().delete(doc_id);
        }
        self.add_document_with_vector_values(table, doc_id, doc, vectors)?;
        Ok(true)
    }

    /// Apply field-level updates without materialising the whole
    /// document. Callers must only use this path when constraints and
    /// referential actions do not need the old or complete new row.
    pub fn patch_document_fields(
        &self,
        table: &str,
        doc_id: DocId,
        updates: &BTreeMap<String, Value>,
        vectors: &BTreeMap<String, Vec<f32>>,
    ) -> Result<bool, SQLError> {
        let vector_values: BTreeMap<String, Vec<Vec<f32>>> = vectors
            .iter()
            .map(|(field, vector)| (field.clone(), vec![vector.clone()]))
            .collect();
        self.patch_document_fields_with_vector_values(table, doc_id, updates, &vector_values)
    }

    pub fn patch_document_fields_with_vector_values(
        &self,
        table: &str,
        doc_id: DocId,
        updates: &BTreeMap<String, Value>,
        vectors: &BTreeMap<String, Vec<Vec<f32>>>,
    ) -> Result<bool, SQLError> {
        let Some(t) = self.table(table) else {
            return Ok(false);
        };

        let fts_fields = t.fts_fields();
        let touches_fts = updates
            .keys()
            .any(|field| fts_fields.iter().any(|fts| fts == field));
        let mut text_fields: BTreeMap<FieldName, String> = BTreeMap::new();
        if touches_fts {
            let store = t.document_store.read();
            for field in &fts_fields {
                let value = updates
                    .get(field)
                    .cloned()
                    .or_else(|| store.get_field(doc_id, field));
                if let Some(Value::Str(text)) = value {
                    text_fields.insert(field.clone(), text);
                }
            }
        }

        let old_indexed = Self::value_indexes_old_values(&t, doc_id);
        let persistent_old = self.persistent_value_indexes_old_values(table, &t, doc_id);
        let mut store = t.document_store.write();
        if !store
            .patch_fields(doc_id, updates)
            .map_err(|err| document_store_write_error(&err))?
        {
            return Ok(false);
        }
        if let Some(old) = persistent_old.as_ref() {
            let new: BTreeMap<String, Value> = old
                .iter()
                .map(|(field, old_value)| {
                    let value = updates.get(field).cloned().unwrap_or(old_value.clone());
                    (field.clone(), value)
                })
                .collect();
            self.persist_value_indexes_apply_write(table, doc_id, Some(&new))?;
        }
        if let Some(old) = old_indexed.as_ref() {
            // New values for the indexed fields: the update wins,
            // untouched fields keep their old value.
            let new: BTreeMap<String, Value> = old
                .iter()
                .map(|(field, old_value)| {
                    let value = updates.get(field).cloned().unwrap_or(old_value.clone());
                    (field.clone(), value)
                })
                .collect();
            Self::value_indexes_apply_write(&t, doc_id, Some(old), Some(&new));
        }
        drop(store);

        if touches_fts {
            let mut index = t.inverted_index.write();
            index.remove_document(doc_id);
            if !text_fields.is_empty() {
                index.add_document(doc_id, text_fields);
            }
        }

        {
            let mut indexes = t.vector_indexes.write();
            for (field, index) in indexes.iter_mut() {
                if !updates.contains_key(field) {
                    continue;
                }
                index.delete(doc_id);
                if let Some(values) = vectors.get(field) {
                    index.add_many(doc_id, values.clone());
                }
            }
        }

        self.mark_column_stats_dirty(table, &t);
        Ok(true)
    }

    pub(crate) fn rewrite_document(
        &self,
        table: &str,
        doc_id: DocId,
        document: Document,
    ) -> Result<(), SQLError> {
        let Some(table_name) = self.resolve_table_name(table) else {
            return Ok(());
        };
        let Some(t) = self.table(table) else {
            return Ok(());
        };
        let vector_fields: Vec<FieldName> = t.vector_indexes.read().keys().cloned().collect();
        let mut vectors: BTreeMap<FieldName, Vec<Vec<f32>>> = BTreeMap::new();
        for field in vector_fields {
            let Some(value) = document.get(&field) else {
                continue;
            };
            if let Some(values) = Self::field_index_vectors(&t, &field, value) {
                vectors.insert(field, values);
            }
        }
        let old_indexed = Self::value_indexes_old_values(&t, doc_id);
        let mut store = t.document_store.write();
        store
            .delete(doc_id)
            .map_err(|err| document_store_write_error(&err))?;
        self.persist_value_indexes_apply_write(&table_name, doc_id, None)?;
        if let Some(old) = old_indexed.as_ref() {
            Self::value_indexes_apply_write(&t, doc_id, Some(old), None);
        }
        drop(store);
        t.inverted_index.write().remove_document(doc_id);
        for idx in t.vector_indexes.write().values_mut() {
            idx.as_mut().delete(doc_id);
        }
        self.add_document_with_vector_values(&table_name, doc_id, document, vectors)
    }

    /// Rewrite a row while a column is being dropped or renamed. The
    /// operation changes field names, not the indexed values: catalog
    /// lifecycle code drops or renames the durable postings afterward.
    /// Maintaining them against the half-updated schema here would replace a
    /// renamed field with NULL before its metadata has moved.
    fn rewrite_document_for_schema_change(
        &self,
        table: &str,
        doc_id: DocId,
        document: Document,
    ) -> Result<(), SQLError> {
        let Some(table_name) = self.resolve_table_name(table) else {
            return Ok(());
        };
        let Some(t) = self.table(table) else {
            return Ok(());
        };
        let vector_fields: Vec<FieldName> = t.vector_indexes.read().keys().cloned().collect();
        let mut vectors: BTreeMap<FieldName, Vec<Vec<f32>>> = BTreeMap::new();
        for field in vector_fields {
            let Some(value) = document.get(&field) else {
                continue;
            };
            if let Some(values) = Self::field_index_vectors(&t, &field, value) {
                vectors.insert(field, values);
            }
        }
        let mut text_fields: BTreeMap<FieldName, String> = BTreeMap::new();
        for field in t.fts_fields() {
            if let Some(Value::Str(value)) = document.get(&field) {
                text_fields.insert(field, value.clone());
            }
        }
        t.document_store
            .write()
            .put(doc_id, document)
            .map_err(|err| document_store_write_error(&err))?;
        {
            let mut index = t.inverted_index.write();
            index.remove_document(doc_id);
            if !text_fields.is_empty() {
                index.add_document(doc_id, text_fields);
            }
        }
        for (field, index) in t.vector_indexes.write().iter_mut() {
            index.delete(doc_id);
            if let Some(values) = vectors.remove(field) {
                index.add_many(doc_id, values);
            }
        }
        self.mark_column_stats_dirty(&table_name, &t);
        Ok(())
    }

    pub fn delete_document(&self, table: &str, doc_id: DocId) -> Result<(), SQLError> {
        let Some(table_name) = self.resolve_table_name(table) else {
            return Ok(());
        };
        let Some(t) = self.table(table) else {
            return Ok(());
        };
        let old_indexed = Self::value_indexes_old_values(&t, doc_id);
        let mut store = t.document_store.write();
        store
            .delete(doc_id)
            .map_err(|err| document_store_write_error(&err))?;
        self.persist_value_indexes_apply_write(&table_name, doc_id, None)?;
        if let Some(old) = old_indexed.as_ref() {
            Self::value_indexes_apply_write(&t, doc_id, Some(old), None);
        }
        drop(store);
        t.inverted_index.write().remove_document(doc_id);
        for idx in t.vector_indexes.write().values_mut() {
            idx.as_mut().delete(doc_id);
        }
        self.mark_column_stats_dirty(&table_name, &t);
        Ok(())
    }

    pub fn document_count(&self, table: &str) -> u64 {
        self.table(table)
            .map_or(0, |t| t.inverted_index.read().doc_count())
    }
}

/// A persistent document-store write failed. Surfacing this as a
/// statement error makes the enclosing transaction roll back, so the
/// on-disk state never keeps a half-applied rewrite.
pub(crate) fn document_store_write_error(err: &StorageBackendError) -> SQLError {
    SQLError::Internal(format!("document store write failed: {err}"))
}
