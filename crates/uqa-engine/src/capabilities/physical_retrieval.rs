//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bind physical retrieval to retained table generations and active statement state.

use crate::{Engine, ScoringMode, TableState};
use std::{collections::BTreeMap, sync::Arc};
use uqa_core::{DocId, ScoredEntry, Value};
use uqa_execution::operator_tree::driver::{
    context::{
        PhysicalDriverContext, PhysicalTextRetrieval, PhysicalVectorRetrieval, RetrievalGraphs,
        RetrievalIndexState, RetrievalIndexes, RetrievalModels, RetrievalRelations,
        RetrievalSnapshots, TextIndexRead, VectorIndexRead,
    },
    PhysicalRetrievalDriver,
};
use uqa_operators::{base::ExecutionContext, TextTopKPlan};
use uqa_sql::{
    ast::{ColumnDef, ColumnType},
    SQLError, SQLParam,
};
use uqa_storage::{document_store::Document, CatalogIndexRow, DocumentStore, StorageBackendResult};

struct TableIndexState(Arc<TableState>);
impl RetrievalIndexState for TableIndexState {
    fn inverted_index(&self) -> TextIndexRead<'_> {
        Box::new(self.0.inverted_index.read())
    }
    fn vector_indexes(&self) -> VectorIndexRead<'_> {
        Box::new(self.0.vector_indexes.read())
    }
}

impl RetrievalRelations for Engine {
    fn try_describe_query_table(
        &self,
        table: &str,
    ) -> StorageBackendResult<Option<Vec<ColumnDef>>> {
        self.try_describe_query_table(table)
    }
    fn has_table(&self, table: &str) -> StorageBackendResult<bool> {
        self.has_table(table)
    }
    fn column_type(&self, table: &str, field: &str) -> StorageBackendResult<Option<ColumnType>> {
        self.column_type(table, field)
    }
    fn table_doc_ids(&self, table: &str) -> Result<Vec<DocId>, SQLError> {
        self.table_doc_ids(table)
    }
    fn get_document_fields(
        &self,
        table: &str,
        ids: &[DocId],
        field: &str,
    ) -> Result<BTreeMap<DocId, Value>, SQLError> {
        self.get_document_fields(table, ids, field)
    }
}

impl RetrievalSnapshots for Engine {
    fn snapshot_context(&self, table: &str) -> Result<Option<ExecutionContext>, SQLError> {
        self.snapshot_context(table)
    }
    fn snapshot_context_with_document_store(
        &self,
        table: &str,
        documents: Arc<dyn DocumentStore>,
    ) -> Result<Option<ExecutionContext>, SQLError> {
        self.snapshot_context_with_document_store(table, documents)
    }
    fn get_documents_with_materialized_projection(
        &self,
        table: &str,
        ids: &[DocId],
        projection: &[String],
    ) -> Result<BTreeMap<DocId, Document>, SQLError> {
        self.get_documents_with_materialized_projection(table, ids, projection)
    }
}

impl RetrievalIndexes for Engine {
    fn query_table_indexes(
        &self,
        table: &str,
    ) -> StorageBackendResult<Option<Box<dyn RetrievalIndexState>>> {
        Ok(self
            .try_query_table(table)?
            .map(|state| Box::new(TableIndexState(state)) as Box<dyn RetrievalIndexState>))
    }
    fn table_indexes(
        &self,
        table: &str,
    ) -> StorageBackendResult<Option<Box<dyn RetrievalIndexState>>> {
        Ok(self
            .table(table)?
            .map(|state| Box::new(TableIndexState(state)) as Box<dyn RetrievalIndexState>))
    }
    fn catalog_index(&self, name: &str) -> StorageBackendResult<Option<CatalogIndexRow>> {
        self.catalog_index(name)
    }
    fn resolve_table_name(&self, table: &str) -> StorageBackendResult<Option<String>> {
        self.resolve_table_name(table)
    }
    fn value_index_scan(
        &self,
        table: &str,
        field: &str,
        predicate: &uqa_core::Predicate,
    ) -> Result<Option<uqa_core::PostingList>, SQLError> {
        self.value_index_scan(table, field, predicate)
    }
}

impl PhysicalTextRetrieval for Engine {
    fn validate_text_search_field(&self, table: &str, field: &str) -> Result<(), SQLError> {
        self.validate_text_search_field(table, field)
    }
    fn fts_fields_for_table(&self, table: &str) -> Result<Vec<String>, SQLError> {
        self.fts_fields_for_table(table)
    }
    fn bayesian_params_for_relation(
        &self,
        table: &str,
        signal_table: &str,
        field: &str,
    ) -> Result<uqa_scoring::BayesianBM25Params, SQLError> {
        self.bayesian_params_for_relation_in_execution(table, signal_table, field)
    }
    fn search_leaf(
        &self,
        table: &str,
        field: &str,
        query: &str,
        mode: &ScoringMode,
        limit: usize,
        top_k: Option<TextTopKPlan>,
    ) -> Result<Vec<ScoredEntry>, SQLError> {
        self.search_leaf(table, field, query, mode, limit, top_k)
    }
}

impl PhysicalVectorRetrieval for Engine {
    fn knn_search_leaf(
        &self,
        table: &str,
        field: &str,
        query: &[f32],
        k: usize,
    ) -> Result<Vec<ScoredEntry>, SQLError> {
        self.knn_search_leaf(table, field, query, k)
    }
}

impl RetrievalGraphs for Engine {
    fn graph_handle(&self, graph: &str) -> Option<Arc<uqa_graph::GraphStoreHandle>> {
        self.graph_handle_in_execution(graph)
    }
}

impl RetrievalModels for Engine {
    fn load_model(&self, name: &str) -> Result<Option<uqa_ml::DeepModel>, SQLError> {
        self.load_model(name)
    }
}

impl Engine {
    pub(crate) fn physical_driver_context(&self) -> PhysicalDriverContext<'_> {
        PhysicalDriverContext {
            runtime: self.query_runtime_view(),
            relations: self,
            snapshots: self,
            indexes: self,
            text: self,
            vector: self,
            graphs: self,
            models: self,
            text_catalog: self,
            text_functions: self,
            documents: self,
            vector_pool: self,
            functions: self,
        }
    }
    pub(crate) fn physical_retrieval_driver<'a>(
        &'a self,
        table: &'a str,
        signal_table: &'a str,
        params: &'a [SQLParam],
    ) -> PhysicalRetrievalDriver<'a> {
        PhysicalRetrievalDriver::new(self.physical_driver_context(), table, signal_table, params)
    }
    pub(crate) fn tree_execution_context(
        &self,
    ) -> uqa_execution::operator_tree::runtime::TreeExecutionContext<'_> {
        uqa_execution::operator_tree::runtime::TreeExecutionContext {
            driver: self.physical_driver_context(),
            optimizer: self,
            transaction: self,
        }
    }
}

impl uqa_execution::operator_tree::runtime::RetrievalPlanOptimizer for Engine {
    fn optimize(
        &self,
        table: &str,
        tree: &uqa_operators::OperatorTree,
    ) -> Result<uqa_operators::OperatorTree, SQLError> {
        Ok(
            uqa_planner::retrieval_planning::query_optimizer(self, table, tree)?
                .optimize(tree.clone()),
        )
    }
}
impl uqa_execution::operator_tree::runtime::RetrievalTransactionState for Engine {
    fn transaction_depth(&self) -> usize {
        self.transaction_depth()
    }
}
