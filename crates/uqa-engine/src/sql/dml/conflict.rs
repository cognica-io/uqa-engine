//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! INSERT conflict resolution, identity extraction, and RETURNING assembly.

use super::{Engine, MutationRowImages, SQLError};
use uqa_execution::OwnedPhysicalRow;
use uqa_sql::ast::ReturningAliases;

#[path = "conflict/inference_adapters.rs"]
mod inference;
pub(in crate::sql) use inference::{prepare_inference_predicate, validate_conflict_target};

pub(in crate::sql) use uqa_execution::mutation::conflict::update::InsertConflictLocks;

pub(in crate::sql) use uqa_sql::semantics::returning::validate_returning_alias_relations;

pub(in crate::sql) fn returning_row_context(
    engine: &Engine,
    table: &str,
    target_qualifier: &str,
    images: MutationRowImages<'_>,
    aliases: &ReturningAliases,
) -> Result<OwnedPhysicalRow, SQLError> {
    uqa_execution::mutation::returning::returning_row_context(
        engine.returning_execution_context(),
        table,
        target_qualifier,
        images,
        aliases,
    )
}

mod returning;
pub(in crate::sql) use returning::{
    build_returning_value_row, dml_command_returning_schema, dml_returning_result,
    dml_returning_result_with_projections, dml_statement_returning_schema, document_supplied_id,
    expanded_returning_projections, returning_expression_schema, returning_target_schema,
    returning_value_context, validate_insert_returning, DmlReturningShape,
    ReturningValueProjectionRow,
};
