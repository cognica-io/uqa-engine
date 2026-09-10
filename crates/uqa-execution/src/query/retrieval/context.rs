//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Session-bound leaf access for retrieval composition.

use uqa_core::{DocId, ScoredEntry};
use uqa_scoring::{BayesianBM25Params, ScoringMode};
use uqa_sql::{expr::EngineHook, semantics::text_indexes::TextMatchCatalog, SQLError};
use uqa_storage::document_store::Document;

pub trait TextRetrieval {
    fn bayesian_params(&self, table: &str, field: &str) -> Result<BayesianBM25Params, SQLError>;
    fn search(
        &self,
        table: &str,
        field: &str,
        query: &str,
        mode: &ScoringMode,
        top_k: usize,
    ) -> Result<Vec<ScoredEntry>, SQLError>;
}

pub trait RetrievalDocuments {
    fn get_document(&self, table: &str, doc_id: DocId) -> Result<Option<Document>, SQLError>;
}

pub trait VectorPoolRetrieval {
    fn query_pool(
        &self,
        table: &str,
        field: &str,
        query_vector: &[f32],
        k: usize,
    ) -> Result<Vec<ScoredEntry>, SQLError>;
}

#[derive(Clone, Copy)]
pub struct TextRetrievalContext<'a> {
    pub catalog: &'a dyn TextMatchCatalog,
    pub text: &'a dyn TextRetrieval,
    pub functions: &'a dyn EngineHook,
}
