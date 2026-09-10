//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Row metadata and scoped expressions used to construct mutation row images.

use crate::{
    catalog::context::CatalogContext, query::relational::QueryExpressionFactory,
    FunctionTypeResolver, RowSchema,
};
use uqa_core::DocId;
use uqa_sql::{ast::ColumnDef, SQLError};
use uqa_storage::DocumentMetadata;

/// Relation metadata observed by mutation row construction.
pub trait MutationRowCatalog: Sync {
    fn column_definitions(&self, table: &str) -> Result<Option<Vec<ColumnDef>>, String>;
    fn column_names(&self, table: &str) -> Result<Vec<String>, String>;
    fn view_schema(&self, name: &str) -> Result<RowSchema, SQLError>;
}

/// Tuple provenance from the statement's read generation and write transaction.
pub trait MutationTupleMetadata: Sync {
    fn document_metadata(
        &self,
        table: &str,
        doc_id: DocId,
    ) -> Result<Option<DocumentMetadata>, SQLError>;
    fn tuple_version_xid(&self) -> Result<u32, SQLError>;
}

#[derive(Clone, Copy)]
pub struct MutationRowContext<'a> {
    pub catalog: CatalogContext<'a>,
    pub relations: &'a dyn MutationRowCatalog,
    pub tuples: &'a dyn MutationTupleMetadata,
}

#[derive(Clone)]
pub struct MutationExpressionContext<'a, S: Clone + 'static> {
    pub types: &'a dyn FunctionTypeResolver,
    pub expressions: &'a dyn QueryExpressionFactory<S>,
}
impl<S: Clone + 'static> Copy for MutationExpressionContext<'_, S> {}
