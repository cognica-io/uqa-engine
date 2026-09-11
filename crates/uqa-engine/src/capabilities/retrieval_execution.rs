//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bind retrieval leaves to the active statement state.

use crate::{Engine, ScoringMode};
use uqa_core::{DocId, ScoredEntry};
use uqa_execution::query::retrieval::context::{
    RetrievalDocuments, TextRetrieval, VectorPoolRetrieval,
};
use uqa_scoring::BayesianBM25Params;
use uqa_sql::{SQLError, SQLParam, ScalarExpr};
use uqa_storage::document_store::Document;

impl TextRetrieval for Engine {
    fn bayesian_params(&self, table: &str, field: &str) -> Result<BayesianBM25Params, SQLError> {
        self.bayesian_params_for_in_execution(table, field)
    }
    fn search(
        &self,
        table: &str,
        field: &str,
        query: &str,
        mode: &ScoringMode,
        top_k: usize,
    ) -> Result<Vec<ScoredEntry>, SQLError> {
        self.search_leaf(table, field, query, mode, top_k, None)
    }
}

impl RetrievalDocuments for Engine {
    fn get_document(&self, table: &str, doc_id: DocId) -> Result<Option<Document>, SQLError> {
        self.get_document(table, doc_id)
    }
}

impl VectorPoolRetrieval for Engine {
    fn query_pool(
        &self,
        table: &str,
        field: &str,
        query_vector: &[f32],
        k: usize,
    ) -> Result<Vec<ScoredEntry>, SQLError> {
        self.query_pool_vector_search_leaf(table, field, query_vector, k)
    }
}

impl Engine {
    pub(crate) fn retrieval_binding(
        &self,
    ) -> uqa_execution::operator_tree::binding::RetrievalBinding<'_> {
        uqa_execution::operator_tree::binding::RetrievalBinding {
            hook: self,
            graphs: self,
        }
    }
}

impl Engine {
    pub(crate) fn retrieval_query_context(
        &self,
    ) -> uqa_execution::operator_tree::query::RetrievalQueryContext<'_> {
        uqa_execution::operator_tree::query::RetrievalQueryContext {
            binding: self.retrieval_binding(),
            trees: self.tree_execution_context(),
            planner: self,
            graphs: self,
        }
    }
}

impl uqa_execution::query::block::context::RelationRetrieval for Engine {
    fn accelerated(
        &self,
        table: &str,
        signal_table: &str,
        predicate: Option<&ScalarExpr>,
        params: &[SQLParam],
    ) -> Result<Option<Vec<ScoredEntry>>, SQLError> {
        self.retrieval_query_context()
            .accelerated(table, signal_table, predicate, params)
    }
    fn optimized(
        &self,
        table: &str,
        predicate: Option<&ScalarExpr>,
        params: &[SQLParam],
    ) -> Result<Option<Vec<ScoredEntry>>, SQLError> {
        self.retrieval_query_context()
            .optimized(table, predicate, params)
    }
    fn function(
        &self,
        table: &str,
        signal_table: &str,
        name: &str,
        args: &[ScalarExpr],
        params: &[SQLParam],
        top_k: Option<usize>,
    ) -> Result<Vec<ScoredEntry>, SQLError> {
        self.retrieval_query_context()
            .function(table, signal_table, name, args, params, top_k)
    }
}
