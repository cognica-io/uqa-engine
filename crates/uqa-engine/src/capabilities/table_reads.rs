//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bind physical scans to the selected table generation.

use crate::TableState;
use uqa_execution::query::table_read::TableRead;
impl TableRead for TableState {
    fn column_definitions(&self) -> Vec<uqa_sql::ast::ColumnDef> {
        self.columns.read().clone()
    }
    fn read_documents(
        &self,
    ) -> parking_lot::RwLockReadGuard<'_, Box<dyn uqa_storage::DocumentStore>> {
        self.document_store.read()
    }
}

use crate::Engine;
use uqa_execution::query::{
    table_read::QueryTableAccess,
    table_sources::{
        retrieval::{DirectVectorRetrieval, RetrievalAccess},
        TableRetrievalContext, TableScanContext,
    },
};
use uqa_sql::{SQLError, SQLParam, ScalarExpr};
impl Engine {
    pub(crate) fn table_scan_context(&self) -> TableScanContext<'_> {
        TableScanContext {
            tables: self,
            runtime: self.query_runtime_view(),
        }
    }
    pub(crate) fn table_retrieval_context(&self) -> TableRetrievalContext<'_> {
        TableRetrievalContext {
            tables: self,
            retrieval: self,
        }
    }
}
impl QueryTableAccess for Engine {
    fn table(&self, name: &str) -> Result<std::sync::Arc<dyn TableRead>, SQLError> {
        self.require_query_table(name)
            .map(|table| table as std::sync::Arc<dyn TableRead>)
    }
    fn command_overlay_changes(
        &self,
        name: &str,
    ) -> Result<
        Option<std::collections::BTreeMap<uqa_core::DocId, Option<uqa_storage::StoredDocument>>>,
        SQLError,
    > {
        self.command_overlay_changes(name)
    }
    fn table_doc_count(&self, name: &str) -> Result<u64, SQLError> {
        self.table_doc_count(name)
    }
}
impl RetrievalAccess for Engine {
    fn direct_vector_retrieval(
        &self,
        predicate: &ScalarExpr,
        params: &[SQLParam],
    ) -> Result<Option<DirectVectorRetrieval>, SQLError> {
        self.retrieval_binding()
            .direct_vector_retrieval(predicate, params)
    }
    fn knn_entries(
        &self,
        table: &str,
        field: &str,
        query: &[f32],
        top_k: usize,
        committed: bool,
    ) -> Result<Vec<crate::ScoredEntry>, SQLError> {
        if committed {
            self.committed_knn_entries(table, field, query, top_k)
        } else {
            self.knn_search_leaf(table, field, query, top_k)
        }
    }
    fn retrieval_entries(
        &self,
        table: &str,
        predicate: &ScalarExpr,
        params: &[SQLParam],
        committed: bool,
    ) -> Result<Option<Vec<crate::ScoredEntry>>, SQLError> {
        if committed {
            self.committed_retrieval_entries(table, predicate, params)
        } else {
            crate::operator_tree_bridge::run_optimised(self, table, Some(predicate), params)
        }
    }
}

impl uqa_execution::query::block::context::QueryDocumentRead for Engine {
    fn document_ids(&self, table: &str) -> Result<Vec<uqa_core::DocId>, SQLError> {
        self.table_doc_ids(table)
    }
    fn document(
        &self,
        table: &str,
        doc_id: uqa_core::DocId,
    ) -> Result<Option<uqa_storage::document_store::Document>, SQLError> {
        self.get_query_document(table, doc_id)
    }
    fn document_fields(
        &self,
        table: &str,
        ids: &[uqa_core::DocId],
        fields: &[&str],
    ) -> Result<std::collections::BTreeMap<uqa_core::DocId, Vec<uqa_core::Value>>, SQLError> {
        self.get_query_document_fields_multi(table, ids, fields)
    }
    fn column_definitions(
        &self,
        table: &str,
    ) -> Result<Option<Vec<uqa_sql::ast::ColumnDef>>, String> {
        self.try_describe_query_table(table)
            .map_err(|error| error.to_string())
    }
    fn command_overlay_active(&self) -> bool {
        self.command_mutation_overlay_active()
    }
}
