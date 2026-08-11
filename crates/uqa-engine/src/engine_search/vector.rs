//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! KNN, calibrated vector, and similarity search.

use super::{storage_sql_error, Engine, SQLError, ScoredEntry};

impl Engine {
    /// Top-`k` nearest neighbors against the named vector field.
    pub(crate) fn knn_search_leaf(
        &self,
        table: &str,
        field: &str,
        query_vector: impl AsRef<[f32]>,
        top_k: usize,
    ) -> Result<Vec<ScoredEntry>, SQLError> {
        if top_k == 0 {
            return Ok(Vec::new());
        }
        let Some(t) = self
            .try_table(table)
            .map_err(|error| storage_sql_error("resolve vector-search table", error))?
        else {
            return Err(SQLError::UnknownTable(table.to_string()));
        };
        let vector_indexes = t.vector_indexes.read();
        let Some(index) = vector_indexes.get(field) else {
            return Err(SQLError::UnknownColumn(field.to_string()));
        };
        let pl = index
            .search_knn(query_vector.as_ref(), top_k)
            .map_err(|error| storage_sql_error("execute KNN search", error))?;
        Ok(Self::rank_top_k(&pl, top_k))
    }

    /// Run KNN and query-pool calibration directly against the registered vector index so metadata validation does not materialize an unrelated full execution context.
    pub(crate) fn query_pool_vector_search_leaf(
        &self,
        table: &str,
        field: &str,
        query_vector: impl AsRef<[f32]>,
        top_k: usize,
    ) -> Result<Vec<ScoredEntry>, SQLError> {
        let query_vector = query_vector.as_ref();
        if query_vector.is_empty() || query_vector.iter().any(|component| !component.is_finite()) {
            return Err(SQLError::TypeMismatch(
                "calibrated vector search requires a non-empty finite query vector".to_string(),
            ));
        }
        if top_k == 0 {
            return Ok(Vec::new());
        }
        let Some(table_state) = self
            .try_table(table)
            .map_err(|error| storage_sql_error("resolve calibrated-vector table", error))?
        else {
            return Err(SQLError::UnknownTable(table.to_string()));
        };
        let indexes = table_state.vector_indexes.read();
        let index = indexes
            .get(field)
            .ok_or_else(|| SQLError::UnknownColumn(field.to_string()))?;
        let raw = index
            .search_knn(query_vector, top_k)
            .map_err(|error| storage_sql_error("execute calibrated-vector KNN", error))?;
        let calibrated = uqa_operators::calibrate_query_pool_postings(
            &raw,
            uqa_operators::RelevantSampleSplit::default(),
            0.5,
        )
        .map_err(|error| storage_sql_error("calibrate vector query pool", error))?;
        Ok(Self::rank_top_k(&calibrated, top_k))
    }

    /// Top-`k` nearest neighbors through the shared operator optimizer and
    /// executor.
    pub fn knn_search(
        &self,
        table: &str,
        field: &str,
        query_vector: impl AsRef<[f32]>,
        top_k: usize,
    ) -> Result<Vec<ScoredEntry>, SQLError> {
        let tree = uqa_operators::OperatorTree::KNN {
            query_vector: query_vector.as_ref().to_vec(),
            k: top_k,
            field: field.to_string(),
        };
        let entries = crate::operator_tree_bridge::execute_scored_tree(self, table, &[], &tree)?;
        Ok(Self::rank_scored_entries_top_k(entries, top_k))
    }

    /// Apply a persisted/offline vector calibration model to a KNN pool.
    ///
    /// Unlike `calibrated_vector_match`, this path never fits parameters from
    /// the current query's top-K results. The caller supplies the current
    /// immutable corpus/index/embedding identity in `target`; it must match
    /// the model provenance exactly. The physical table, field, index kind,
    /// dimensions, and candidate K are validated again at execution time.
    pub fn calibrated_vector_search_with_model(
        &self,
        table: &str,
        field: &str,
        query_vector: impl AsRef<[f32]>,
        model: &uqa_scoring::VectorCalibrationModel,
        target: &uqa_scoring::VectorCalibrationTarget,
    ) -> Result<Vec<ScoredEntry>, SQLError> {
        model
            .validate_for(target)
            .map_err(|error| SQLError::TypeMismatch(error.to_string()))?;
        let table_name = self
            .try_resolve_table_name(table)
            .map_err(|error| storage_sql_error("resolve calibrated-vector table", error))?
            .ok_or_else(|| SQLError::UnknownTable(table.to_string()))?;
        let expected_index_id = format!("{table_name}.{field}");
        if target.corpus_id != table_name {
            return Err(SQLError::TypeMismatch(format!(
                "vector calibration corpus_id {:?} does not match table {:?}",
                target.corpus_id, table_name
            )));
        }
        if target.index_id != expected_index_id {
            return Err(SQLError::TypeMismatch(format!(
                "vector calibration index_id {:?} does not match physical index {:?}",
                target.index_id, expected_index_id
            )));
        }

        let table = self
            .try_table(&table_name)
            .map_err(|error| storage_sql_error("resolve calibrated-vector table", error))?
            .ok_or_else(|| SQLError::UnknownTable(table_name.clone()))?;
        let indexes = table.vector_indexes.read();
        let index = indexes
            .get(field)
            .ok_or_else(|| SQLError::UnknownColumn(field.to_string()))?;
        if target.index_kind != index.index_kind() {
            return Err(SQLError::TypeMismatch(format!(
                "vector calibration index kind {:?} does not match {:?}",
                target.index_kind,
                index.index_kind()
            )));
        }
        if target.dimensions != index.dimensions() {
            return Err(SQLError::VectorDimMismatch {
                expected: index.dimensions() as usize,
                actual: target.dimensions as usize,
            });
        }
        let raw = index
            .search_knn(query_vector.as_ref(), target.candidate_k)
            .map_err(|error| storage_sql_error("execute calibrated-vector KNN", error))?;
        let mut calibrated = Vec::with_capacity(raw.len());
        for entry in &raw {
            if !entry.payload.score.is_finite() || !(-1.0..=1.0).contains(&entry.payload.score) {
                return Err(SQLError::Internal(format!(
                    "calibrated-vector KNN returned invalid cosine score {} for document {}",
                    entry.payload.score, entry.doc_id
                )));
            }
            let probability = model
                .calibrate_one(1.0 - entry.payload.score, target)
                .map_err(|error| SQLError::Internal(error.to_string()))?;
            calibrated.push(ScoredEntry {
                doc_id: entry.doc_id,
                score: probability,
            });
        }
        Ok(Self::rank_scored_entries_top_k(
            calibrated,
            target.candidate_k,
        ))
    }

    /// All documents whose cosine similarity to `query_vector` is at least
    /// `threshold`.
    pub fn vector_similarity_search(
        &self,
        table: &str,
        field: &str,
        query_vector: Vec<f32>,
        threshold: f32,
    ) -> Result<Vec<ScoredEntry>, SQLError> {
        let tree = uqa_operators::OperatorTree::VectorSimilarity {
            query_vector,
            threshold,
            field: field.to_string(),
        };
        let mut out = crate::operator_tree_bridge::execute_scored_tree(self, table, &[], &tree)?;
        out.sort_by(|a, b| {
            b.score
                .total_cmp(&a.score)
                .then_with(|| a.doc_id.cmp(&b.doc_id))
        });
        Ok(out)
    }
}
