//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Construct physical joins using SQL-owned positional binding.

use uqa_execution::{JoinOutput, PhysicalOperator, RowSchema};
use uqa_sql::{ast::JoinKind, SQLError};

pub(in crate::sql) use uqa_sql::semantics::{
    join_alias_input_schemas, join_using_layout, join_using_predicate, resolve_join_using,
    ResolvedJoinUsing,
};

pub(in crate::sql) fn shape_join_using_output<'a>(
    operator: Box<dyn PhysicalOperator + 'a>,
    kind: JoinKind,
    left: &RowSchema,
    right: &RowSchema,
    using: &ResolvedJoinUsing,
) -> Result<Box<dyn PhysicalOperator + 'a>, SQLError> {
    let (columns, aliases) = join_using_layout(kind, left, right, using)?;
    JoinOutput::try_new(operator, columns, aliases)
        .map(|output| Box::new(output) as Box<dyn PhysicalOperator + 'a>)
        .map_err(super::super::select::physical_exec_error)
}
