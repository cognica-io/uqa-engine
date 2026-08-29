//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Persistent store rebinding and vector extraction.

use super::{
    AnalyzerPhase, BTreeMap, Document, Engine, FieldName, SQLError, StorageBackendError,
    StorageBackendResult, TableState, Value,
};
use crate::VectorIndexSpec;

impl Engine {
    pub(crate) fn rebind_persistent_table_stores(
        &self,
        table_name: &str,
        table: &TableState,
    ) -> StorageBackendResult<()> {
        let Some(backend) = self.storage.backend.as_ref() else {
            return Ok(());
        };
        let analyzer = table.analyzer.read().clone();
        *table.document_store.write() = backend.document_store(table_name);
        table
            .doc_count_dirty
            .store(true, std::sync::atomic::Ordering::Release);
        Self::value_indexes_clear(table);
        *table.inverted_index.write() = backend.inverted_index(table_name, analyzer);

        let analyzer_rows: Vec<(String, String, String)> = self
            .durable
            .table_field_analyzers
            .read()
            .iter()
            .filter(|((table, _), _)| table == table_name)
            .map(|((_, field), (analyzer, phase))| (field.clone(), analyzer.clone(), phase.clone()))
            .collect();
        for (field, analyzer_name, phase) in analyzer_rows {
            let analyzer = self
                .resolve_analyzer(&analyzer_name)
                .map_err(StorageBackendError::Other)?;
            let phase = if phase.eq_ignore_ascii_case("index") {
                AnalyzerPhase::Index
            } else if phase.eq_ignore_ascii_case("search") {
                AnalyzerPhase::Search
            } else {
                AnalyzerPhase::Both
            };
            table
                .inverted_index
                .write()
                .set_field_analyzer(&field, analyzer, phase)
                .map_err(StorageBackendError::Other)?;
        }

        let vector_fields: Vec<(String, u32)> = table
            .vector_indexes
            .read()
            .iter()
            .map(|(field, idx)| (field.clone(), idx.dimensions()))
            .collect();
        let mut rebound = BTreeMap::new();
        for (field, dimensions) in vector_fields {
            let spec = self
                .vector_index_spec_for_column(table_name, &field)?
                .unwrap_or(VectorIndexSpec::BruteForce);
            let idx = self.build_vector_index_for_restore(table_name, &field, dimensions, spec)?;
            rebound.insert(field, idx);
        }
        *table.vector_indexes.write() = rebound;
        Ok(())
    }

    pub(crate) fn field_index_vectors(
        table: &TableState,
        field: &str,
        value: &Value,
    ) -> Result<Option<Vec<Vec<f32>>>, SQLError> {
        if matches!(value, Value::Null) {
            return Ok(None);
        }
        let ty = table
            .columns
            .read()
            .iter()
            .find(|column| column.name == field)
            .map(|column| column.ty.clone());
        match ty {
            Some(uqa_sql::ast::ColumnType::Tensor(dim)) => {
                let tensor = uqa_sql::expr::value_to_tensor(value)?;
                for vector in &tensor {
                    crate::sql::validate_vector_dimensions(dim, vector.len())?;
                }
                Ok(Some(tensor))
            }
            Some(uqa_sql::ast::ColumnType::Vector(dim)) => {
                let vector = uqa_sql::expr::value_to_vector(value)?;
                crate::sql::validate_vector_dimensions(dim, vector.len())?;
                Ok(Some(vec![vector]))
            }
            _ => Ok(Some(vec![uqa_sql::expr::value_to_vector(value)?])),
        }
    }

    /// Derive a complete replacement snapshot for every registered vector
    /// field from the document. Missing/NULL fields are represented by an
    /// empty vector list so replacement clears stale index entries instead of
    /// accidentally preserving them.
    pub(crate) fn document_vector_values(
        table: &TableState,
        document: &Document,
    ) -> Result<BTreeMap<FieldName, Vec<Vec<f32>>>, SQLError> {
        let fields = table
            .vector_indexes
            .read()
            .keys()
            .cloned()
            .collect::<Vec<_>>();
        let mut vectors = BTreeMap::new();
        for field in fields {
            let values = match document.get(&field) {
                Some(value) => Self::field_index_vectors(table, &field, value)?.unwrap_or_default(),
                None => Vec::new(),
            };
            vectors.insert(field, values);
        }
        Ok(vectors)
    }
}
