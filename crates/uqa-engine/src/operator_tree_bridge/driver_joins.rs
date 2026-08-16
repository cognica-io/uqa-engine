//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Text, vector, hybrid, and cross-paradigm join execution.

use super::{
    first_structured_field, require_graph_name, require_shared_structured_field,
    require_shared_vector_field, require_text_field, require_vector_field, DriverResult,
    EngineDriver, GeneralizedPostingList, OperatorTree, PostingEntry, PostingList, SQLError,
};

impl EngineDriver<'_> {
    pub(super) fn execute_text_similarity_join(
        &self,
        left: &OperatorTree,
        right: &OperatorTree,
        threshold: f64,
    ) -> DriverResult<GeneralizedPostingList> {
        if !threshold.is_finite() || !(0.0..=1.0).contains(&threshold) {
            return Err(SQLError::TypeMismatch(format!(
                "TextSimilarityJoin.threshold must be finite and in [0, 1], got {threshold}"
            )));
        }
        let left_field = require_text_field(left, "TextSimilarityJoin.left")?;
        let right_field = require_text_field(right, "TextSimilarityJoin.right")?;
        let left_source = self.execute_posting_node(left)?;
        let right_source = self.execute_posting_node(right)?;
        let left = self.prepare_join_operand(&left_source, &left_field, "_join_text")?;
        let right = self.prepare_join_operand(&right_source, &right_field, "_join_text")?;
        uqa_joins::TextSimilarityJoin::new(
            left.entries(),
            right.entries(),
            "_join_text",
            "_join_text",
        )
        .threshold(threshold)
        .execute()
        .map_err(|error| SQLError::Internal(format!("execute TextSimilarityJoin: {error}")))
    }

    pub(super) fn execute_vector_similarity_join(
        &self,
        left: &OperatorTree,
        right: &OperatorTree,
        threshold: f64,
    ) -> DriverResult<GeneralizedPostingList> {
        if !threshold.is_finite() || !(-1.0..=1.0).contains(&threshold) {
            return Err(SQLError::TypeMismatch(format!(
                "VectorSimilarityJoin.threshold must be finite and in [-1, 1], got {threshold}"
            )));
        }
        let left_field = require_vector_field(left, "VectorSimilarityJoin.left")?;
        let right_field = require_vector_field(right, "VectorSimilarityJoin.right")?;
        let left_source = self.execute_posting_node(left)?;
        let right_source = self.execute_posting_node(right)?;
        let left = self.prepare_join_operand(&left_source, &left_field, "_join_vector")?;
        let right = self.prepare_join_operand(&right_source, &right_field, "_join_vector")?;
        uqa_joins::VectorSimilarityJoin::new(
            left.entries(),
            right.entries(),
            "_join_vector",
            "_join_vector",
        )
        .threshold(threshold)
        .execute()
        .map_err(|error| SQLError::Internal(format!("execute VectorSimilarityJoin: {error}")))
    }

    pub(super) fn execute_hybrid_join(
        &self,
        left: &OperatorTree,
        right: &OperatorTree,
    ) -> DriverResult<GeneralizedPostingList> {
        let structured_field = require_shared_structured_field(left, right, "HybridJoin")?;
        let vector_field = require_shared_vector_field(left, right, "HybridJoin")?;
        let left_result = self.execute_posting_node(left)?;
        let right_result = self.execute_posting_node(right)?;
        let left_keyed =
            self.prepare_join_operand(&left_result, &structured_field.0, "_join_key")?;
        let left_result =
            self.prepare_join_operand(&left_keyed, &vector_field.0, "_join_vector")?;
        let right_keyed =
            self.prepare_join_operand(&right_result, &structured_field.1, "_join_key")?;
        let right_result =
            self.prepare_join_operand(&right_keyed, &vector_field.1, "_join_vector")?;
        uqa_joins::HybridJoin::new(
            left_result.entries(),
            right_result.entries(),
            "_join_key",
            "_join_vector",
        )
        .execute()
        .map_err(|error| SQLError::Internal(format!("execute HybridJoin: {error}")))
    }

    pub(super) fn execute_cross_paradigm_join(
        &self,
        left: &OperatorTree,
        right: &OperatorTree,
    ) -> DriverResult<GeneralizedPostingList> {
        let graph = require_graph_name(left, "CrossParadigmJoin.left")?;
        let vertex_field = first_structured_field(left)
            .or_else(|| first_structured_field(right))
            .ok_or_else(|| {
                SQLError::TypeMismatch(
                    "CrossParadigmJoin operands do not identify a join property".to_string(),
                )
            })?;
        let doc_field = first_structured_field(right).unwrap_or_else(|| vertex_field.clone());
        let left_result = self.execute_posting_node(left)?;
        let right_source = self.execute_posting_node(right)?;
        let right_result =
            self.prepare_join_operand(&right_source, &doc_field, "_join_document")?;
        self.with_graph(&graph, |store| {
            uqa_joins::CrossParadigmJoin::new(
                left_result.entries(),
                right_result.entries(),
                store,
                &vertex_field,
                "_join_document",
            )
            .execute()
            .map_err(|error| SQLError::Internal(format!("execute CrossParadigmJoin: {error}")))
        })
    }

    pub(super) fn prepare_join_operand(
        &self,
        source: &PostingList,
        field: &str,
        alias: &str,
    ) -> DriverResult<PostingList> {
        let lookup_doc_ids = source
            .entries()
            .iter()
            .filter(|entry| !entry.payload.fields.contains_key(field))
            .map(|entry| entry.doc_id)
            .collect::<Vec<_>>();
        let projected = if lookup_doc_ids.is_empty() {
            std::collections::BTreeMap::new()
        } else {
            self.require_column(field)?;
            self.engine
                .get_document_fields(self.table, &lookup_doc_ids, field)?
        };
        let mut entries = Vec::with_capacity(source.len());
        for entry in source.entries() {
            let mut payload = entry.payload.clone();
            let value = if let Some(value) = payload.fields.get(field) {
                value.clone()
            } else if let Some(value) = projected.get(&entry.doc_id) {
                value.clone()
            } else {
                return Err(SQLError::Internal(format!(
                    "join operand references document {} missing from table `{}`",
                    entry.doc_id, self.table
                )));
            };
            payload.fields.insert(alias.to_string(), value);
            entries.push(PostingEntry::new(entry.doc_id, payload));
        }
        Ok(PostingList::from_sorted_unchecked(entries))
    }
}
