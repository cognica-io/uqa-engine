//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{fields, variant, Node, Part, SQLError};
use crate::ast::{
    AssignmentStep, AssignmentTarget, AssignmentTargets, DeleteStmt, Expr, InsertStmt, MergeStmt,
    MergeTargetColumnBinding, MergeWhen, MultipleAssignmentTargets, OnConflict, OnConflictAction,
    UpdateStmt,
};

image_struct!(InsertStmt {
    table,
    target_relation_bound,
    target_qualifier,
    target_alias,
    include_descendants,
    columns,
    overriding,
    with,
    rows,
    select_source,
    on_conflict,
    returning,
    returning_aliases,
});
image_struct!(UpdateStmt {
    table,
    target_relation_bound,
    target_qualifier,
    target_alias,
    include_descendants,
    assignments,
    r#where,
    with,
    from,
    returning,
    returning_aliases,
});
image_struct!(DeleteStmt {
    table,
    target_relation_bound,
    target_qualifier,
    target_alias,
    include_descendants,
    r#where,
    with,
    using,
    returning,
    returning_aliases,
});
image_struct!(MergeStmt {
    with,
    target,
    target_qualifier,
    target_alias,
    target_column_bindings,
    include_descendants,
    source,
    join_condition,
    when_clauses,
    returning,
    returning_aliases,
});
image_struct!(MergeTargetColumnBinding {
    object_id,
    attribute_number,
    domain_dependencies
});
image_enum!(MergeWhen {
    0 => UpdateMatched { condition, assignments },
    1 => DeleteMatched { condition },
    2 => UpdateNotMatchedBySource { condition, assignments },
    3 => DeleteNotMatchedBySource { condition },
    4 => InsertNotMatched { condition, columns, overriding, values },
    5 => NothingMatched { condition },
    6 => NothingNotMatched { condition },
    7 => NothingNotMatchedBySource { condition },
});
image_struct!(OnConflict {
    predicate,
    constraint,
    conflict_columns,
    expressions,
    action
});
image_enum!(OnConflictAction { 0 => Nothing, 1 => Update { assignments, r#where } });
image_struct!(AssignmentTarget {
    column,
    indirection
});
image_enum!(AssignmentStep<Expr> { 0 => Field(value), 1 => Index(value), 2 => Slice { lower, upper } });
image_struct!(MultipleAssignmentTargets<Expr> {
    targets,
    source_positions,
    source_width
});
image_enum!(AssignmentTargets { 0 => Single(value), 1 => Multiple(value) });

impl Node for (AssignmentTargets, Expr) {
    fn part(&self, position: usize) -> Result<Option<Part<'_>>, SQLError> {
        Ok(fields(position, &[&self.0, &self.1]))
    }
}
