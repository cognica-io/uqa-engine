//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Catalog, snapshot and leaf inputs for physical retrieval execution.

use crate::query::retrieval::context::{RetrievalDocuments, TextRetrieval, VectorPoolRetrieval};
use std::{collections::BTreeMap, ops::Deref, sync::Arc};
use uqa_core::{DocId, ScoredEntry, Value};
use uqa_graph::GraphStoreHandle;
use uqa_operators::{base::ExecutionContext, TextTopKPlan};
use uqa_scoring::{BayesianBM25Params, ScoringMode};
use uqa_sql::{
    ast::{ColumnDef, ColumnType},
    expr::EngineHook,
    semantics::text_indexes::TextMatchCatalog,
    SQLError,
};
use uqa_storage::{
    document_store::Document, CatalogIndexRow, DocumentStore, InvertedIndex, StorageBackendResult,
    VectorIndex,
};

pub type TextIndexRead<'a> = Box<dyn Deref<Target = Box<dyn InvertedIndex>> + 'a>;
pub type VectorIndexRead<'a> = Box<dyn Deref<Target = BTreeMap<String, Box<dyn VectorIndex>>> + 'a>;

pub trait RetrievalIndexState: Send + Sync {
    fn inverted_index(&self) -> TextIndexRead<'_>;
    fn vector_indexes(&self) -> VectorIndexRead<'_>;
}

pub trait RetrievalRelations: Sync {
    fn try_describe_query_table(&self, table: &str)
        -> StorageBackendResult<Option<Vec<ColumnDef>>>;
    fn has_table(&self, table: &str) -> StorageBackendResult<bool>;
    fn column_type(&self, table: &str, field: &str) -> StorageBackendResult<Option<ColumnType>>;
    fn table_doc_ids(&self, table: &str) -> Result<Vec<DocId>, SQLError>;
    fn get_document_fields(
        &self,
        table: &str,
        ids: &[DocId],
        field: &str,
    ) -> Result<BTreeMap<DocId, Value>, SQLError>;
}

pub trait RetrievalSnapshots: Sync {
    fn snapshot_context(&self, table: &str) -> Result<Option<ExecutionContext>, SQLError>;
    fn snapshot_context_with_document_store(
        &self,
        table: &str,
        documents: Arc<dyn DocumentStore>,
    ) -> Result<Option<ExecutionContext>, SQLError>;
    fn get_documents_with_materialized_projection(
        &self,
        table: &str,
        ids: &[DocId],
        projection: &[String],
    ) -> Result<BTreeMap<DocId, Document>, SQLError>;
}

pub trait RetrievalIndexes: Sync {
    fn query_table_indexes(
        &self,
        table: &str,
    ) -> StorageBackendResult<Option<Box<dyn RetrievalIndexState>>>;
    fn table_indexes(
        &self,
        table: &str,
    ) -> StorageBackendResult<Option<Box<dyn RetrievalIndexState>>>;
    fn catalog_index(&self, name: &str) -> StorageBackendResult<Option<CatalogIndexRow>>;
    fn resolve_table_name(&self, table: &str) -> StorageBackendResult<Option<String>>;
    fn value_index_scan(
        &self,
        table: &str,
        field: &str,
        predicate: &uqa_core::Predicate,
    ) -> Result<Option<uqa_core::PostingList>, SQLError>;
}

pub trait PhysicalTextRetrieval: Sync {
    fn validate_text_search_field(&self, table: &str, field: &str) -> Result<(), SQLError>;
    fn fts_fields_for_table(&self, table: &str) -> Result<Vec<String>, SQLError>;
    fn bayesian_params_for_relation(
        &self,
        table: &str,
        signal_table: &str,
        field: &str,
    ) -> Result<BayesianBM25Params, SQLError>;
    fn search_leaf(
        &self,
        table: &str,
        field: &str,
        query: &str,
        mode: &ScoringMode,
        limit: usize,
        top_k: Option<TextTopKPlan>,
    ) -> Result<Vec<ScoredEntry>, SQLError>;
}

pub trait PhysicalVectorRetrieval: Sync {
    fn knn_search_leaf(
        &self,
        table: &str,
        field: &str,
        query: &[f32],
        k: usize,
    ) -> Result<Vec<ScoredEntry>, SQLError>;
}

pub trait RetrievalGraphs: Sync {
    fn graph_handle(&self, graph: &str) -> Option<Arc<GraphStoreHandle>>;
}

pub trait RetrievalModels: Sync {
    fn load_model(&self, name: &str) -> Result<Option<uqa_ml::DeepModel>, SQLError>;
}

#[derive(Clone, Copy)]
pub struct PhysicalDriverContext<'a> {
    pub runtime: crate::query::runtime::QueryRuntimeView<'a>,
    pub relations: &'a dyn RetrievalRelations,
    pub snapshots: &'a dyn RetrievalSnapshots,
    pub indexes: &'a dyn RetrievalIndexes,
    pub text: &'a dyn PhysicalTextRetrieval,
    pub vector: &'a dyn PhysicalVectorRetrieval,
    pub graphs: &'a dyn RetrievalGraphs,
    pub models: &'a dyn RetrievalModels,
    pub text_catalog: &'a (dyn TextMatchCatalog + Sync),
    pub text_functions: &'a (dyn TextRetrieval + Sync),
    pub documents: &'a (dyn RetrievalDocuments + Sync),
    pub vector_pool: &'a (dyn VectorPoolRetrieval + Sync),
    pub functions: &'a (dyn EngineHook + Sync),
}
