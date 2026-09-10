//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Supply engine hierarchy metadata and active expression evaluation for partition routing.

use crate::Engine;
use uqa_sql::{ResultRow as Document, SQLError, SQLParam};

pub(in crate::sql) fn validate_hash_partition_spec(
    engine: &Engine,
    spec: &uqa_sql::ast::PartitionSpec,
    columns: &[uqa_sql::ast::ColumnDef],
) -> Result<(), SQLError> {
    uqa_sql::semantics::partition::validate_hash_partition_spec(
        &engine.partition_context(),
        spec,
        columns,
    )
}

pub(in crate::sql) fn validate_new_partition_bound(
    engine: &Engine,
    parent: &str,
    bound: &uqa_sql::ast::PartitionBound,
) -> Result<(), SQLError> {
    uqa_sql::semantics::partition::validate_new_partition_bound(
        &engine.partition_context(),
        parent,
        bound,
    )
}

pub(in crate::sql) fn prospective_partition_bound_accepts_document(
    engine: &Engine,
    parent: &str,
    bound: &uqa_sql::ast::PartitionBound,
    document: &Document,
) -> Result<bool, SQLError> {
    uqa_sql::semantics::partition::prospective_partition_bound_accepts_document(
        &engine.partition_context(),
        parent,
        bound,
        document,
    )
}

pub(in crate::sql) fn partition_insert_target(
    engine: &Engine,
    requested_table: &str,
    document: &Document,
    params: &[SQLParam],
    include_descendants: bool,
) -> Result<String, SQLError> {
    uqa_sql::semantics::partition::partition_insert_target(
        &engine.partition_context(),
        requested_table,
        document,
        params,
        include_descendants,
    )
}
