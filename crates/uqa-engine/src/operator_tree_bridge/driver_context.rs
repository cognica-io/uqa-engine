//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Opaque-node handling, runtime validation, query features, and execution context.

use super::{
    first_text_signal, operator_execution_error, BTreeMap, ColumnType, DocId, DriverResult,
    EngineDriver, OperatorTree, Payload, PostingEntry, PostingList, SQLError, Value,
};

impl EngineDriver<'_> {
    pub(super) fn execute_opaque(
        kind: &str,
        _children: &[OperatorTree],
        _meta: &BTreeMap<String, Value>,
    ) -> DriverResult<PostingList> {
        Err(SQLError::UnknownFunction(format!("operator::{kind}")))
    }

    /// Build the `n_query_features=6` vector that attention fusers
    /// expect. When the IR carries a non-empty explicit vector it wins
    /// (test fixtures); otherwise the driver extracts the canonical
    /// `[mean_idf, max_idf, min_idf, coverage, query_length,
    /// vocab_overlap]` vector from the table's inverted-index stats
    /// against the first text-bearing signal it can find.
    pub(super) fn attention_query_features(
        &self,
        signals: &[OperatorTree],
        explicit: &[f64],
    ) -> DriverResult<Vec<f64>> {
        if !explicit.is_empty() {
            return Ok(explicit.to_vec());
        }
        let Some(table_state) = self
            .engine
            .table(self.table)
            .map_err(|error| operator_execution_error("resolve attention table", error))?
        else {
            return Err(SQLError::UnknownTable(self.table.to_string()));
        };
        let idx_guard = table_state.inverted_index.read();
        let index_stats = idx_guard
            .stats()
            .map_err(|error| operator_execution_error("index statistics", error))?;
        if let Some((field, query)) = first_text_signal(signals) {
            let analyzer = idx_guard.get_search_analyzer(&field);
            let terms = analyzer
                .analyze(&query)
                .map_err(|error| operator_execution_error("attention query analysis", error))?;
            return Ok(
                uqa_fusion::extract_query_features(&index_stats, &terms, Some(&field)).to_vec(),
            );
        }
        Ok(vec![0.0; uqa_fusion::N_QUERY_FEATURES])
    }

    pub(super) fn require_column(&self, field: &str) -> DriverResult<()> {
        let columns = self
            .engine
            .describe_table(self.table)
            .map_err(|error| operator_execution_error("resolve operator table", error))?;
        let Some(columns) = columns else {
            return Err(SQLError::UnknownTable(self.table.to_string()));
        };
        // `create_default_table` intentionally creates a schema-less dynamic
        // document table: its registered FTS fields and arbitrary stored
        // fields remain valid operator inputs. SQL-created typed tables have
        // a non-empty declared schema and retain strict unknown-column errors.
        if !columns.is_empty() && !columns.iter().any(|column| column.name == field) {
            return Err(SQLError::UnknownColumn(field.to_string()));
        }
        Ok(())
    }

    pub(super) fn require_vector_query(
        &self,
        field: &str,
        query_vector: &[f32],
    ) -> DriverResult<()> {
        if !self
            .engine
            .has_table(self.table)
            .map_err(|error| operator_execution_error("resolve vector table", error))?
        {
            return Err(SQLError::UnknownTable(self.table.to_string()));
        }
        let declared_type = self
            .engine
            .column_type(self.table, field)
            .map_err(|error| operator_execution_error("resolve vector column", error))?;
        if let Some(column_type) = declared_type.as_ref() {
            if !matches!(column_type, ColumnType::Vector(_) | ColumnType::Tensor(_)) {
                return Err(SQLError::TypeMismatch(format!(
                    "vector search requires a VECTOR or TENSOR field, but {field:?} is {column_type:?}"
                )));
            }
        }
        let table = self
            .engine
            .table(self.table)
            .map_err(|error| operator_execution_error("resolve vector table", error))?
            .ok_or_else(|| SQLError::UnknownTable(self.table.to_string()))?;
        let indexes = table.vector_indexes.read();
        let index = indexes
            .get(field)
            .ok_or_else(|| match declared_type.as_ref() {
                Some(ColumnType::Vector(_) | ColumnType::Tensor(_)) => SQLError::Unsupported(
                    format!("vector field {field:?} has no physical vector index"),
                ),
                Some(column_type) => SQLError::Internal(format!(
                    "non-vector field {field:?} with type {column_type:?} passed vector validation"
                )),
                None => SQLError::UnknownColumn(field.to_string()),
            })?;
        let indexed_dimensions = index.dimensions() as usize;
        let expected_dimensions = match declared_type.as_ref() {
            Some(ColumnType::Vector(dimensions) | ColumnType::Tensor(dimensions)) => {
                *dimensions as usize
            }
            Some(column_type) => {
                return Err(SQLError::Internal(format!(
                    "non-vector field {field:?} with type {column_type:?} passed vector validation"
                )))
            }
            // `create_default_table` is the intentionally schema-less
            // embedded API. In that mode the registered vector index is
            // the field's durable schema declaration.
            None => indexed_dimensions,
        };
        if expected_dimensions != indexed_dimensions {
            return Err(SQLError::Internal(format!(
                "vector schema for {field:?} declares {expected_dimensions} dimensions but its index has {indexed_dimensions}"
            )));
        }
        if query_vector.len() != expected_dimensions {
            return Err(SQLError::TypeMismatch(format!(
                "vector query for {field:?} has {} dimensions, expected {expected_dimensions}",
                query_vector.len()
            )));
        }
        if query_vector.iter().any(|value| !value.is_finite()) {
            return Err(SQLError::TypeMismatch(format!(
                "vector query for {field:?} must contain only finite values"
            )));
        }
        Ok(())
    }

    pub(super) fn bridge_context(&self) -> DriverResult<uqa_operators::base::ExecutionContext> {
        if self.table.is_empty() {
            return Ok(uqa_operators::base::ExecutionContext::new());
        }
        self.engine
            .snapshot_context(self.table)?
            .ok_or_else(|| SQLError::UnknownTable(self.table.to_string()))
    }

    /// Build an operator context whose document snapshot exposes the requested
    /// virtual generated columns. Only those columns are evaluated, and only
    /// for the candidate documents the consuming operator can visit.
    pub(super) fn bridge_context_for_projection(
        &self,
        doc_ids: &[DocId],
        fields: &[&str],
    ) -> DriverResult<uqa_operators::base::ExecutionContext> {
        let columns = self
            .engine
            .describe_table(self.table)
            .map_err(|error| operator_execution_error("resolve projected operator table", error))?
            .ok_or_else(|| SQLError::UnknownTable(self.table.to_string()))?;
        let projection = fields
            .iter()
            .map(|field| (*field).to_string())
            .collect::<Vec<_>>();
        if !crate::engine_generated::projection_contains_virtual_generated_column(
            &columns,
            &projection,
        ) {
            return self.bridge_context();
        }

        let documents =
            self.engine
                .get_documents_with_virtual_projection(self.table, doc_ids, &projection)?;
        let mut store = uqa_storage::MemoryDocumentStore::new();
        for (doc_id, document) in documents {
            uqa_storage::DocumentStore::put(&mut store, doc_id, document).map_err(|error| {
                operator_execution_error("build projected operator snapshot", error)
            })?;
        }
        self.engine
            .snapshot_context_with_document_store(self.table, std::sync::Arc::new(store))?
            .ok_or_else(|| SQLError::UnknownTable(self.table.to_string()))
    }

    pub(super) fn facet_vector_inline(
        &self,
        vec_pl: &PostingList,
        facet_field: &str,
    ) -> DriverResult<PostingList> {
        use std::collections::BTreeMap;
        self.require_column(facet_field)?;
        let doc_ids = vec_pl
            .entries()
            .iter()
            .map(|entry| entry.doc_id)
            .collect::<Vec<_>>();
        let values = self
            .engine
            .get_document_fields(self.table, &doc_ids, facet_field)?;
        let mut counts: BTreeMap<String, u64> = BTreeMap::new();
        for entry in vec_pl.entries() {
            let Some(value) = values.get(&entry.doc_id) else {
                return Err(SQLError::Internal(format!(
                    "vector facet candidate {} is missing from table `{}`",
                    entry.doc_id, self.table
                )));
            };
            if !matches!(value, Value::Null) {
                let key = match value {
                    Value::Str(s) => s.clone(),
                    Value::Int(n) => n.to_string(),
                    Value::Float(f) => format!("{f}"),
                    Value::Bool(b) => b.to_string(),
                    other => format!("{other:?}"),
                };
                let count = counts.entry(key).or_insert(0);
                *count = count.checked_add(1).ok_or_else(|| {
                    SQLError::Internal("vector facet count overflowed u64".to_string())
                })?;
            }
        }
        let mut entries: Vec<PostingEntry> = Vec::with_capacity(counts.len());
        for (i, (value, count)) in counts.into_iter().enumerate() {
            let count_value = i64::try_from(count).map_err(|_| {
                SQLError::Internal(format!(
                    "vector facet count {count} exceeds the SQL BIGINT range"
                ))
            })?;
            if count > 9_007_199_254_740_992 {
                return Err(SQLError::Internal(format!(
                    "vector facet count {count} cannot be represented exactly as an f64 score"
                )));
            }
            let bucket_id = DocId::try_from(i).map_err(|_| {
                SQLError::Internal(format!(
                    "vector facet bucket index {i} exceeds the document-id range"
                ))
            })?;
            let mut fields = std::collections::BTreeMap::new();
            fields.insert(
                "_facet_field".to_string(),
                Value::Str(facet_field.to_string()),
            );
            fields.insert("_facet_value".to_string(), Value::Str(value));
            fields.insert("_facet_count".to_string(), Value::Int(count_value));
            entries.push(PostingEntry::new(
                bucket_id,
                Payload {
                    positions: Vec::new(),
                    score: count as f64,
                    fields,
                },
            ));
        }
        Ok(PostingList::from_sorted_unchecked(entries))
    }
}
