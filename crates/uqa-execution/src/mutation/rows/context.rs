//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Row metadata and scoped expressions used to construct mutation row images.

use crate::{
    catalog::context::CatalogContext, query::relational::QueryExpressionFactory,
    FunctionTypeResolver,
};
use uqa_core::DocId;
use uqa_sql::SQLError;
use uqa_storage::DocumentMetadata;

pub use uqa_sql::semantics::mutation_rows::MutationRowCatalog;

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
