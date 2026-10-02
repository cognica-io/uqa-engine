//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Route rows to partitions and check partition constraints, reporting a row they reject as `PostgreSQL` does.

use super::{violations::partition_rejection_error, ConstraintContext, ConstraintStatement};
use uqa_sql::{
    semantics::partition::{
        partition_constraint_accepts_row, route_partition_insert, PartitionRejection,
        PartitionRoute,
    },
    SQLError, SQLParam,
};
use uqa_storage::document_store::Document;

/// The table that stores a row that `statement` inserts through `requested_table` (`ExecFindPartition`). A partition that the statement names stores the row itself, and the row's other constraints check its partition constraint after them.
pub fn partition_insert_target(
    context: ConstraintContext<'_>,
    statement: ConstraintStatement<'_>,
    requested_table: &str,
    document: &Document,
    params: &[SQLParam],
    include_descendants: bool,
) -> Result<String, SQLError> {
    match route_partition_insert(
        &context.partitions,
        requested_table,
        document,
        params,
        include_descendants,
    )? {
        PartitionRoute::Target(table) => Ok(table),
        PartitionRoute::Rejected(rejection) => Err(partition_rejection_error(
            context, statement, rejection, document,
        )),
    }
}

/// Check that a row `statement` writes satisfies the partition constraint of `table` (`ExecPartitionCheck`).
pub fn validate_partition_constraint(
    context: ConstraintContext<'_>,
    statement: ConstraintStatement<'_>,
    table: &str,
    document: &Document,
    params: &[SQLParam],
) -> Result<(), SQLError> {
    if partition_constraint_accepts_row(&context.partitions, table, document, params)? {
        return Ok(());
    }
    Err(partition_rejection_error(
        context,
        statement,
        PartitionRejection::Constraint {
            relation: table.to_string(),
        },
        document,
    ))
}
