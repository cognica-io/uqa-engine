//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bind retrieval leaves to public or already-active statement state.

use crate::{Engine, ScoringMode};
use uqa_core::{DocId, ScoredEntry};
use uqa_execution::query::retrieval::context::{
    RetrievalDocuments, TextRetrieval, TextRetrievalContext, VectorPoolRetrieval,
};
use uqa_scoring::BayesianBM25Params;
use uqa_sql::SQLError;
use uqa_storage::document_store::Document;

#[derive(Clone, Copy)]
pub(crate) enum TextRetrievalSession<'a> {
    Public(&'a Engine),
    InExecution(&'a Engine),
}

impl TextRetrievalSession<'_> {
    pub(crate) fn context(&self) -> TextRetrievalContext<'_> {
        let (Self::Public(engine) | Self::InExecution(engine)) = self;
        TextRetrievalContext {
            catalog: *engine,
            text: self,
            functions: *engine,
        }
    }
}

impl TextRetrieval for TextRetrievalSession<'_> {
    fn bayesian_params(&self, table: &str, field: &str) -> Result<BayesianBM25Params, SQLError> {
        match self {
            Self::Public(engine) => engine.bayesian_params_for(table, field),
            Self::InExecution(engine) => engine.bayesian_params_for_in_execution(table, field),
        }
    }
    fn search(
        &self,
        table: &str,
        field: &str,
        query: &str,
        mode: &ScoringMode,
        top_k: usize,
    ) -> Result<Vec<ScoredEntry>, SQLError> {
        match self {
            Self::Public(engine) => engine.search(table, field, query, mode, top_k),
            Self::InExecution(engine) => engine.search_leaf(table, field, query, mode, top_k, None),
        }
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
