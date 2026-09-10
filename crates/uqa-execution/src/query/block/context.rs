//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Statement-bound row access and retrieval execution for query blocks.

use std::collections::BTreeMap;
use uqa_core::{DocId, ScoredEntry, Value};
use uqa_sql::{ast::ColumnDef, SQLError, SQLParam, ScalarExpr};
use uqa_storage::document_store::Document;

pub trait QueryDocumentRead: Sync {
    fn document_ids(&self, table: &str) -> Result<Vec<DocId>, SQLError>;
    fn document(&self, table: &str, doc_id: DocId) -> Result<Option<Document>, SQLError>;
    fn document_fields(
        &self,
        table: &str,
        ids: &[DocId],
        fields: &[&str],
    ) -> Result<BTreeMap<DocId, Vec<Value>>, SQLError>;
    fn column_definitions(&self, table: &str) -> Result<Option<Vec<ColumnDef>>, String>;
    fn command_overlay_active(&self) -> bool;
}

/// Retrieval planning and execution against the caller's selected relation generation.
pub trait RelationRetrieval: Sync {
    fn accelerated(
        &self,
        table: &str,
        signal_table: &str,
        predicate: Option<&ScalarExpr>,
        params: &[SQLParam],
    ) -> Result<Option<Vec<ScoredEntry>>, SQLError>;
    fn optimized(
        &self,
        table: &str,
        predicate: Option<&ScalarExpr>,
        params: &[SQLParam],
    ) -> Result<Option<Vec<ScoredEntry>>, SQLError>;
    fn function(
        &self,
        table: &str,
        signal_table: &str,
        name: &str,
        args: &[ScalarExpr],
        params: &[SQLParam],
        top_k: Option<usize>,
    ) -> Result<Vec<ScoredEntry>, SQLError>;
}
