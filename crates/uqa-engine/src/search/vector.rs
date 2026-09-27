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
            .try_query_table(table)
            .map_err(|error| storage_sql_error("resolve vector-search table", error))?
        else {
            return Err(SQLError::UnknownTable(table.to_string()));
        };
        let vector_indexes = t.vector_indexes.read();
        let Some(index) = vector_indexes.get(field) else {
            return Err(SQLError::UnknownColumn(field.to_string()));
        };
        let pl = uqa_execution::serializable::vector::search_knn(
            index,
            self.serializable_table_read(table)?.as_ref(),
            &t.columns.snapshot(),
            field,
            query_vector.as_ref(),
            top_k,
        )
        .map_err(|error| storage_sql_error("execute KNN search", error))?;
        Ok(uqa_scoring::rank_top_k(&pl, top_k))
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
            .try_query_table(table)
            .map_err(|error| storage_sql_error("resolve calibrated-vector table", error))?
        else {
            return Err(SQLError::UnknownTable(table.to_string()));
        };
        let indexes = table_state.vector_indexes.read();
        let index = indexes
            .get(field)
            .ok_or_else(|| SQLError::UnknownColumn(field.to_string()))?;
        let raw = uqa_execution::serializable::vector::search_knn(
            index,
            self.serializable_table_read(table)?.as_ref(),
            &table_state.columns.snapshot(),
            field,
            query_vector,
            top_k,
        )
        .map_err(|error| storage_sql_error("execute calibrated-vector KNN", error))?;
        let calibrated = uqa_operators::calibrate_query_pool_postings(
            &raw,
            uqa_operators::RelevantSampleSplit::default(),
            0.5,
        )
        .map_err(|error| storage_sql_error("calibrate vector query pool", error))?;
        Ok(uqa_scoring::rank_top_k(&calibrated, top_k))
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
        self.with_direct_table_read(table, |engine, name, _| {
            let tree = uqa_operators::OperatorTree::KNN {
                query_vector: query_vector.as_ref().to_vec(),
                k: top_k,
                field: field.to_string(),
            };
            let entries =
                crate::operator_tree_bridge::execute_scored_tree(engine, name, table, &[], &tree)?;
            Ok(uqa_scoring::rank_scored_entries_top_k(entries, top_k))
        })
    }

    /// Obtain the actual `DiskANN` corpus and physical-generation versions for fitting a fixed calibration model. Embedding identity remains the caller's contract. This metadata read does not execute KNN or register a serializable vector predicate.
    pub fn diskann_calibration_target(
        &self,
        table: &str,
        field: &str,
        embedding_model_id: &str,
        embedding_model_version: &str,
        candidate_k: usize,
    ) -> Result<uqa_scoring::VectorCalibrationTarget, SQLError> {
        self.with_direct_table_read(table, |engine, table_name, table| {
            let indexes = table.vector_indexes.read();
            let index = indexes
                .get(field)
                .ok_or_else(|| SQLError::UnknownColumn(field.into()))?;
            let control = engine.query_retention_control()?;
            uqa_execution::query::vector_calibration::diskann_target(
                index,
                table_name,
                field,
                (embedding_model_id, embedding_model_version),
                candidate_k,
                &control,
            )
        })
    }

    /// Apply a persisted/offline vector calibration model to a KNN pool.
    ///
    /// Unlike `calibrated_vector_match`, this path never fits parameters from the current query's top-K results. The target must match the model provenance, physical table/field, index kind, dimensions and candidate K. For `DiskANN`, obtain actual corpus/generation versions with [`Self::diskann_calibration_target`]; execution revalidates them against the same retained index used for KNN. Embedding identity and other index methods' version labels remain explicit caller contracts.
    pub fn calibrated_vector_search_with_model(
        &self,
        table: &str,
        field: &str,
        query_vector: impl AsRef<[f32]>,
        model: &uqa_scoring::VectorCalibrationModel,
        target: &uqa_scoring::VectorCalibrationTarget,
    ) -> Result<Vec<ScoredEntry>, SQLError> {
        self.with_direct_table_read(table, |engine, table_name, table| {
            uqa_execution::query::vector_calibration::validate_names(
                model, target, table_name, field,
            )?;

            let indexes = table.vector_indexes.read();
            let index = indexes
                .get(field)
                .ok_or_else(|| SQLError::UnknownColumn(field.to_string()))?;
            let control = engine.query_retention_control()?;
            let retained = if index.index_kind() == "diskann" {
                Some(index.snapshot_with_control(&control).map_err(|error| {
                    storage_sql_error("retain calibrated-vector selection", error)
                })?)
            } else {
                None
            };
            let index = retained.as_deref().unwrap_or(index);
            uqa_execution::query::vector_calibration::validate_index(index, target, &control)?;
            let raw = uqa_execution::serializable::vector::search_knn(
                index,
                engine.serializable_table_state_read(table)?.as_ref(),
                &table.columns.snapshot(),
                field,
                query_vector.as_ref(),
                target.candidate_k,
            )
            .map_err(|error| storage_sql_error("execute calibrated-vector KNN", error))?;
            model
                .calibrate_postings(&raw, target)
                .map_err(|error| SQLError::Internal(error.to_string()))
        })
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
        self.with_direct_table_read(table, |engine, name, _| {
            let tree = uqa_operators::OperatorTree::VectorSimilarity {
                query_vector,
                threshold,
                field: field.to_string(),
            };
            let mut out =
                crate::operator_tree_bridge::execute_scored_tree(engine, name, table, &[], &tree)?;
            out.sort_by(|a, b| {
                b.score
                    .total_cmp(&a.score)
                    .then_with(|| a.doc_id.cmp(&b.doc_id))
            });
            Ok(out)
        })
    }
}
