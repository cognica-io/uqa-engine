//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Supply engine hierarchy metadata and active expression evaluation for partition routing.

use crate::Engine;
use uqa_sql::{ResultRow as Document, SQLError};

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
