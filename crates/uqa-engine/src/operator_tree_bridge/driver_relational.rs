//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Document-set, scoring, aggregation, and hybrid execution.

use super::{
    graph_execution_error, operator_execution_error, static_operator, BTreeMap, BTreeSet, DocId,
    DriverResult, EngineDriver, OperatorOutput, OperatorTree, OperatorTreeDriver, Payload,
    PostingEntry, PostingList, SQLError,
};

impl EngineDriver<'_> {
    pub(super) fn execute_intersect(&self, parts: &[OperatorTree]) -> DriverResult<OperatorOutput> {
        let membership_only = parts.iter().all(OperatorTree::is_membership_only);
        let mut iter = self.execute_output_branches(parts)?.into_iter();
        let Some(first) = iter.next() else {
            return Ok(PostingList::new().into());
        };
        iter.try_fold(first, |acc, next| match (acc, next) {
            (OperatorOutput::Posting(left), OperatorOutput::Posting(right)) => {
                let intersection = if membership_only {
                    left.merge_support_intersection_owned(&right)
                } else {
                    left.merge_intersection_owned(&right)
                };
                Ok(OperatorOutput::Posting(intersection))
            }
            (OperatorOutput::Graph(left), OperatorOutput::Graph(right)) => left
                .merge_intersection(&right)
                .map(OperatorOutput::Graph)
                .map_err(|error| graph_execution_error("GraphIntersect", error)),
            (OperatorOutput::Generalized(left), OperatorOutput::Generalized(right)) => {
                Ok(OperatorOutput::Generalized(left.merge_intersection(&right)))
            }
            _ => Err(SQLError::TypeMismatch(
                "Intersect operands must use the same posting-list carrier".to_string(),
            )),
        })
    }

    pub(super) fn execute_union(&self, parts: &[OperatorTree]) -> DriverResult<OperatorOutput> {
        let mut iter = self.execute_output_branches(parts)?.into_iter();
        let Some(first) = iter.next() else {
            return Ok(PostingList::new().into());
        };
        iter.try_fold(first, |acc, next| match (acc, next) {
            (OperatorOutput::Posting(left), OperatorOutput::Posting(right)) => {
                Ok(OperatorOutput::Posting(left.merge_union(&right)))
            }
            (OperatorOutput::Graph(left), OperatorOutput::Graph(right)) => left
                .merge_union(&right)
                .map(OperatorOutput::Graph)
                .map_err(|error| graph_execution_error("GraphUnion", error)),
            (OperatorOutput::Generalized(left), OperatorOutput::Generalized(right)) => {
                Ok(OperatorOutput::Generalized(left.merge_union(&right)))
            }
            _ => Err(SQLError::TypeMismatch(
                "Union operands must use the same posting-list carrier".to_string(),
            )),
        })
    }

    pub(super) fn execute_complement(&self, inner: &OperatorTree) -> DriverResult<PostingList> {
        if !self
            .engine
            .has_table(self.table)
            .map_err(|error| operator_execution_error("resolve complement table", error))?
        {
            return Err(SQLError::UnknownTable(self.table.to_string()));
        }
        let inner_pl = self.execute_posting_node(inner)?;
        let included: BTreeSet<DocId> = inner_pl.entries().iter().map(|e| e.doc_id).collect();
        let mut entries: Vec<PostingEntry> = Vec::new();
        for doc_id in self.engine.table_doc_ids(self.table)? {
            if !included.contains(&doc_id) {
                entries.push(PostingEntry::new(doc_id, Payload::default()));
            }
        }
        entries.sort_by_key(|e| e.doc_id);
        Ok(PostingList::from_sorted_unchecked(entries))
    }

    pub(super) fn execute_composed(&self, parts: &[OperatorTree]) -> DriverResult<OperatorOutput> {
        let mut result = OperatorOutput::Posting(PostingList::new());
        for part in parts {
            result = self.execute_node(part)?;
        }
        Ok(result)
    }

    pub(super) fn execute_facet(
        &self,
        field: &str,
        source: Option<&OperatorTree>,
    ) -> DriverResult<PostingList> {
        use uqa_operators::{FacetOperator, Operator};

        self.require_column(field)?;
        let source = source
            .map(|child| self.execute_posting_node(child).map(static_operator))
            .transpose()?;
        let op = FacetOperator::new(field, source);
        op.execute(&self.bridge_context()?)
            .map_err(|error| operator_execution_error("Facet", error))
    }

    pub(super) fn execute_score(
        &self,
        scorer: &uqa_operators::ScorerRef,
        source: &OperatorTree,
        query_terms: &[String],
        field: &str,
    ) -> DriverResult<PostingList> {
        use uqa_operators::{Operator, ScoreOperator};

        self.require_column(field)?;
        let source = static_operator(self.execute_posting_node(source)?);
        let op = ScoreOperator::new(scorer.clone(), source, query_terms.to_vec(), field);
        op.execute(&self.bridge_context()?)
            .map_err(|error| operator_execution_error("Score", error))
    }

    pub(super) fn execute_vector_similarity(
        &self,
        query_vector: &[f32],
        threshold: f32,
        field: &str,
    ) -> DriverResult<PostingList> {
        use uqa_operators::{Operator, VectorSimilarityOperator};

        self.require_vector_query(field, query_vector)?;
        if !threshold.is_finite() || !(-1.0..=1.0).contains(&threshold) {
            return Err(SQLError::TypeMismatch(format!(
                "VectorSimilarity.threshold must be finite and in [-1, 1], got {threshold}"
            )));
        }
        let op = VectorSimilarityOperator::new(query_vector.to_vec(), threshold, field);
        op.execute(&self.bridge_context()?)
            .map_err(|error| operator_execution_error("VectorSimilarity", error))
    }

    pub(super) fn execute_aggregate(
        &self,
        source: Option<&OperatorTree>,
        field: &str,
        monoid: &std::sync::Arc<dyn uqa_operators::AggregationMonoid>,
    ) -> DriverResult<PostingList> {
        use uqa_operators::{AggregateOperator, Operator};

        self.require_column(field)?;
        let source = source
            .map(|child| self.execute_posting_node(child).map(static_operator))
            .transpose()?;
        let op = AggregateOperator::new(source, field, monoid.clone());
        op.execute(&self.bridge_context()?)
            .map_err(|error| operator_execution_error("Aggregate", error))
    }

    pub(super) fn execute_group_by(
        &self,
        source: &OperatorTree,
        group_field: &str,
        agg_field: &str,
        monoid: &std::sync::Arc<dyn uqa_operators::AggregationMonoid>,
    ) -> DriverResult<PostingList> {
        use uqa_operators::{GroupByOperator, Operator};

        self.require_column(group_field)?;
        self.require_column(agg_field)?;
        let source = static_operator(self.execute_posting_node(source)?);
        let op = GroupByOperator::new(source, group_field, agg_field, monoid.clone());
        op.execute(&self.bridge_context()?)
            .map_err(|error| operator_execution_error("GroupBy", error))
    }

    pub(super) fn execute_hybrid_text_vector(
        &self,
        term_op: &OperatorTree,
        vector_op: &OperatorTree,
        alpha: f64,
    ) -> DriverResult<PostingList> {
        if !alpha.is_finite() || !(0.0..=1.0).contains(&alpha) {
            return Err(SQLError::TypeMismatch(format!(
                "HybridTextVector.alpha must be finite and in [0, 1], got {alpha}"
            )));
        }
        let text = self.execute_posting_node(term_op)?;
        let vector = self.execute_posting_node(vector_op)?;
        let text_scores: BTreeMap<DocId, f64> = text
            .entries()
            .iter()
            .map(|entry| (entry.doc_id, entry.payload.score))
            .collect();
        let vector_scores: BTreeMap<DocId, f64> = vector
            .entries()
            .iter()
            .map(|entry| (entry.doc_id, entry.payload.score))
            .collect();
        let intersection = text.merge_intersection_owned(&vector);
        let entries = intersection
            .entries()
            .iter()
            .map(|entry| {
                let text_score = text_scores.get(&entry.doc_id).copied().ok_or_else(|| {
                    SQLError::Internal(format!(
                        "HybridTextVector consistency error: intersection candidate {} is missing from the text score map",
                        entry.doc_id
                    ))
                })?;
                let vector_score = vector_scores
                .get(&entry.doc_id)
                .copied()
                    .ok_or_else(|| {
                        SQLError::Internal(format!(
                            "HybridTextVector consistency error: intersection candidate {} is missing from the vector score map",
                            entry.doc_id
                        ))
                    })?;
                let mut scored = entry.clone();
                scored.payload.score = alpha * text_score + (1.0 - alpha) * vector_score;
                Ok(scored)
            })
            .collect::<DriverResult<Vec<_>>>()?;
        Ok(PostingList::from_sorted_unchecked(entries))
    }

    pub(super) fn execute_semantic_filter(
        &self,
        source: &OperatorTree,
        vector_op: &OperatorTree,
    ) -> DriverResult<PostingList> {
        let source = self.execute_posting_node(source)?;
        let vector = self.execute_posting_node(vector_op)?;
        Ok(source.merge_intersection_owned(&vector))
    }
}
