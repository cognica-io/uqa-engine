//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{
    AnalyzerPhase, Arc, BTreeMap, DocId, Document, Engine, FieldName, SQLError, TableState, Value,
};

impl Engine {
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
        let Some(name) = self.resolve_table_name(name) else {
            return false;
        };
        let removed = self.tables.write().remove(&name).is_some();
        if !removed {
            return false;
        }
        // Sweep every related per-table registry so catalog state
        // does not outlive the table.
        self.table_field_analyzers
            .write()
            .retain(|(t, _), _| t != &name);
        self.catalog_indexes
            .write()
            .retain(|_, row| row.table_name != name);
        if let Some(catalog) = self.catalog.as_ref() {
            let _ = catalog.drop_table(&name);
            let _ = catalog.purge_table_data(&name);
            let _ = catalog.drop_table_field_analyzers(&name);
            let _ = catalog.drop_catalog_indexes_for_table(&name);
        }
        true
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
        let Some(table_name) = self.resolve_table_name(table) else {
            return;
        };
        let Some(t) = self.table(table) else {
            return;
        };
        if t.columns.read().iter().any(|c| c.name == column.name) {
            return;
        }
        t.columns.write().push(column);
        self.mark_column_stats_dirty(&table_name, &t);
        if self.is_persistent() {
            self.save_table_schema(&table_name, &t);
        }
    }

    pub fn drop_column(&self, table: &str, column: &str) {
        let Some(table_name) = self.resolve_table_name(table) else {
            return;
        };
        let Some(t) = self.table(table) else {
            return;
        };
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
        let ids: Vec<DocId> = t.document_store.read().snapshot().doc_ids();
        for doc_id in ids {
            let Some(mut doc) = t.document_store.read().get(doc_id) else {
                continue;
            };
            if doc.remove(column).is_some() {
                self.rewrite_document(&table_name, doc_id, doc);
            }
        }
        if self.is_persistent() {
            self.save_table_schema(&table_name, &t);
        }
        self.mark_column_stats_dirty(&table_name, &t);
    }

    pub fn rename_column(&self, table: &str, from: &str, to: &str) {
        let Some(table_name) = self.resolve_table_name(table) else {
            return;
        };
        let Some(t) = self.table(table) else {
            return;
        };
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
                self.rewrite_document(&table_name, doc_id, doc);
            }
        }
        if let Some(dimensions) = vector_dimensions {
            self.create_vector_field(&table_name, to, dimensions);
        }
        if self.is_persistent() {
            self.save_table_schema(&table_name, &t);
        }
        self.mark_column_stats_dirty(&table_name, &t);
    }

    pub fn rename_table(&self, from: &str, to: &str) -> bool {
        let Some(from) = self.resolve_table_name(from) else {
            return false;
        };
        let to = self.relation_name_for_create(to);
        let mut tables = self.tables.write();
        if tables.contains_key(&to) {
            return false;
        }
        let Some(state) = tables.remove(&from) else {
            return false;
        };
        tables.insert(to.clone(), state.clone());
        drop(tables);
        if let Some(catalog) = self.catalog.as_ref() {
            let _ = catalog.drop_table(&from);
        }
        if self.is_persistent() {
            self.save_table_schema(&to, &state);
        }
        self.mark_column_stats_dirty(&to, &state);
        true
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
    /// columns map to the unique-constraint target. The UQA-RS implementation
    /// scans every document because the planner does not yet support
    /// secondary unique indexes; the lookup is still correct for
    /// small / medium tables, which is where UPSERT is most useful.
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
        let snap = t.document_store.read().snapshot();
        for doc_id in snap.doc_ids() {
            let Some(doc) = snap.get(doc_id) else {
                continue;
            };
            let mut all_match = true;
            for (col, want) in conflict_columns.iter().zip(values.iter()) {
                let got = doc.get(col).cloned().unwrap_or(Value::Null);
                if &got != want {
                    all_match = false;
                    break;
                }
            }
            if all_match {
                return Some(doc_id);
            }
        }
        None
    }

    /// Apply per-column updates to an existing document. Mirrors the
    /// `DO UPDATE SET col = expr` branch of an ON CONFLICT clause.
    /// Returns whether the row was updated; `false` when the document
    /// no longer exists.
    pub fn update_document_fields(
        &self,
        table: &str,
        doc_id: DocId,
        updates: BTreeMap<String, Value>,
        vectors: BTreeMap<String, Vec<f32>>,
    ) -> bool {
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
    ) -> bool {
        let Some(t) = self.table(table) else {
            return false;
        };
        let Some(mut doc) = t.document_store.read().get(doc_id) else {
            return false;
        };
        for (k, v) in updates {
            doc.insert(k, v);
        }
        // Re-add the document so the inverted index picks up the new
        // text fields.
        t.document_store.write().delete(doc_id);
        t.inverted_index.write().remove_document(doc_id);
        for idx in t.vector_indexes.write().values_mut() {
            idx.as_mut().delete(doc_id);
        }
        self.add_document_with_vector_values(table, doc_id, doc, vectors);
        true
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
    ) -> bool {
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
    ) -> bool {
        let Some(t) = self.table(table) else {
            return false;
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

        if !t.document_store.write().patch_fields(doc_id, updates) {
            return false;
        }

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
        true
    }

    pub(crate) fn rewrite_document(&self, table: &str, doc_id: DocId, document: Document) {
        let Some(table_name) = self.resolve_table_name(table) else {
            return;
        };
        let Some(t) = self.table(table) else {
            return;
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
        t.document_store.write().delete(doc_id);
        t.inverted_index.write().remove_document(doc_id);
        for idx in t.vector_indexes.write().values_mut() {
            idx.as_mut().delete(doc_id);
        }
        self.add_document_with_vector_values(&table_name, doc_id, document, vectors);
    }

    pub fn delete_document(&self, table: &str, doc_id: DocId) {
        let Some(table_name) = self.resolve_table_name(table) else {
            return;
        };
        let Some(t) = self.table(table) else {
            return;
        };
        t.document_store.write().delete(doc_id);
        t.inverted_index.write().remove_document(doc_id);
        for idx in t.vector_indexes.write().values_mut() {
            idx.as_mut().delete(doc_id);
        }
        self.mark_column_stats_dirty(&table_name, &t);
    }

    pub fn document_count(&self, table: &str) -> u64 {
        self.table(table)
            .map_or(0, |t| t.inverted_index.read().doc_count())
    }
}
