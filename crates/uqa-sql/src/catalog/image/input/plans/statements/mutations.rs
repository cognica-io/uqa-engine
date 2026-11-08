//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{invalid, Body, Empty, Field, Input, PlanMemory, SQLError, Step};
use crate::ast::{
    AssignmentStep, AssignmentTarget, AssignmentTargets, DeleteStmt, Expr, InsertStmt, MergeStmt,
    MergeWhen, MultipleAssignmentTargets, OnConflict, OnConflictAction, UpdateStmt,
};

syntax_struct!(InsertStmt {
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
    returning_aliases
});
syntax_struct!(UpdateStmt {
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
    returning_aliases
});
syntax_struct!(DeleteStmt {
    table,
    target_relation_bound,
    target_qualifier,
    target_alias,
    include_descendants,
    r#where,
    with,
    using,
    returning,
    returning_aliases
});
syntax_struct!(MergeStmt {
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
    returning_aliases
});
syntax_struct!(OnConflict {
    predicate,
    constraint,
    conflict_columns,
    expressions,
    action
});
syntax_struct!(AssignmentTarget {
    column,
    indirection
});
syntax_struct!(MultipleAssignmentTargets<Expr> { targets, source_positions, source_width });

syntax_enum!(OnConflictAction, OnConflictAction::Nothing;
    0 => Nothing, 1 => Update { assignments, r#where },
);
syntax_enum!(AssignmentStep<Expr>, AssignmentStep::Field(String::new());
    0 => Field(value), 1 => Index(value), 2 => Slice { lower, upper },
);
syntax_enum!(AssignmentTargets, AssignmentTargets::Single(Empty::empty());
    0 => Single(value), 1 => Multiple(value),
);
syntax_enum!(MergeWhen, MergeWhen::NothingMatched { condition: None };
    0 => UpdateMatched { condition, assignments }, 1 => DeleteMatched { condition },
    2 => UpdateNotMatchedBySource { condition, assignments }, 3 => DeleteNotMatchedBySource { condition },
    4 => InsertNotMatched { condition, columns, overriding, values },
    5 => NothingMatched { condition }, 6 => NothingNotMatched { condition },
    7 => NothingNotMatchedBySource { condition },
);

impl Empty for (AssignmentTargets, Expr) {
    fn empty() -> Self {
        (Empty::empty(), Expr::Star)
    }
    fn is_flat(&self) -> bool {
        !self.0.needs_retirement() && !self.1.needs_retirement()
    }
}
impl Body for (AssignmentTargets, Expr) {
    fn new(_: &mut PlanMemory) -> Result<Self, SQLError> {
        Ok(<Self as Empty>::empty())
    }
    fn field(&mut self, position: usize) -> Option<&mut dyn Field> {
        match position {
            0 => Some(&mut self.0),
            1 => Some(&mut self.1),
            _ => None,
        }
    }
}
