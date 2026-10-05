//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Rewrite stored column values after a declaration changes their SQL type.
use crate::mutation::constraints::context::MutationRead;
use crate::mutation::publication::DocumentVectors;
use std::collections::BTreeMap;
use uqa_core::{DocId, Value};
use uqa_sql::assignment::{columns::AssignmentColumnCatalog, AssignmentContext};
use uqa_sql::semantics::partition::PartitionExpressions;
use uqa_sql::SQLError;
/// Publish converted fields through the caller's storage and index update path.
pub trait ColumnRewritePublication {
    fn update_fields(
        &self,
        table: &str,
        id: DocId,
        values: BTreeMap<String, Value>,
        vectors: DocumentVectors,
    ) -> Result<bool, SQLError>;
}
pub struct ColumnRewriteContext<'a> {
    pub cancellation: &'a uqa_core::CancellationToken,
    pub columns: &'a dyn AssignmentColumnCatalog,
    pub reads: &'a dyn MutationRead,
    pub types: &'a dyn AssignmentContext,
    pub expressions: &'a dyn PartitionExpressions,
    pub writes: &'a dyn ColumnRewritePublication,
}
pub mod rows;

pub mod backfill;
pub mod generated;

pub mod addition;

pub mod alteration;

pub mod deletion;

pub mod removal;
