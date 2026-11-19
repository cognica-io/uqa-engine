//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{fields, invalid, variant, Node, Part, SQLError};
use crate::{
    ast::{AssignmentStep, AssignmentTarget, AssignmentTargets, MultipleAssignmentTargets},
    plan::{
        AssignmentPlan, CommandPlan, ConflictActionPlan, ConflictPlan, DeletePlan, InsertPlan,
        MergePlan, MergeWhenPlan, RuleValuesInput, UpdatePlan, ViewCheckPlan, ViewRuleInsertPlan,
        ViewRuleReturningPlan, ViewRuleUpdatePlan,
    },
    ScalarExpr,
};

image_struct!(InsertPlan {
    table,
    target_relation_bound,
    relations_bound,
    statement_privilege_subject,
    target_privilege_subject,
    target_qualifier,
    target_alias,
    include_descendants,
    columns,
    overriding,
    ctes,
    rows,
    source,
    rule_values_input,
    on_conflict,
    returning,
    returning_aliases,
    subqueries,
    view_checks,
    view_rule_relations,
    view_rule_insert_plans,
    view_rule_returning,
});
image_struct!(UpdatePlan {
    table,
    target_relation_bound,
    relations_bound,
    statement_privilege_subject,
    target_privilege_subject,
    target_qualifier,
    target_alias,
    include_descendants,
    assignments,
    predicate,
    ctes,
    source,
    returning,
    returning_aliases,
    subqueries,
    view_checks,
    view_rule_relations,
    view_rule_update_plans,
    view_rule_returning,
});
image_struct!(DeletePlan {
    table,
    target_relation_bound,
    relations_bound,
    statement_privilege_subject,
    target_privilege_subject,
    target_qualifier,
    target_alias,
    include_descendants,
    predicate,
    ctes,
    source,
    returning,
    returning_aliases,
    subqueries,
    view_rule_relations,
    view_rule_returning,
});
image_struct!(MergePlan {
    ctes,
    target,
    statement_privilege_subject,
    target_privilege_subject,
    target_qualifier,
    target_alias,
    include_descendants,
    target_predicate,
    source,
    join_condition,
    when_clauses,
    returning,
    returning_aliases,
    subqueries,
    view_checks,
});
image_struct!(RuleValuesInput {
    width,
    inline,
    defaults,
    unresolved,
    written
});
image_struct!(ConflictPlan {
    predicate,
    constraint,
    conflict_columns,
    expressions,
    action
});
image_struct!(AssignmentPlan { target, value });
image_struct!(AssignmentTarget<ScalarExpr> { column, indirection });
image_struct!(MultipleAssignmentTargets<ScalarExpr> { targets, source_positions, source_width });
image_struct!(ViewCheckPlan { view, predicate });
image_struct!(ViewRuleReturningPlan {
    relation,
    target_qualifier,
    returning,
    aliases,
    subqueries
});
image_struct!(ViewRuleInsertPlan {
    relation,
    supplied_columns,
    input_columns
});
image_struct!(ViewRuleUpdatePlan {
    relation,
    assigned_columns,
    input_columns
});
image_enum!(AssignmentTargets<ScalarExpr> { 0 => Single(value), 1 => Multiple(value) });
image_enum!(AssignmentStep<ScalarExpr> { 0 => Field(value), 1 => Index(value), 2 => Slice { lower, upper } });
image_enum!(ConflictActionPlan { 0 => Nothing, 1 => Update { assignments, predicate } });
image_enum!(MergeWhenPlan {
    0 => UpdateMatched { condition, assignments },
    1 => DeleteMatched { condition },
    2 => UpdateNotMatchedBySource { condition, assignments },
    3 => DeleteNotMatchedBySource { condition },
    4 => InsertNotMatched { condition, columns, overriding, values },
    5 => NothingMatched { condition },
    6 => NothingNotMatched { condition },
    7 => NothingNotMatchedBySource { condition },
});

impl Node for CommandPlan {
    fn part(&self, position: usize) -> Result<Option<Part<'_>>, SQLError> {
        // These are exactly CteBody's four command constructors. Utility commands cannot be query CTE bodies.
        Ok(match self {
            Self::Insert(value) => variant(position, 0, &[value]),
            Self::Update(value) => variant(position, 1, &[value]),
            Self::Delete(value) => variant(position, 2, &[value]),
            Self::Merge(value) => variant(position, 3, &[value]),
            _ => return Err(invalid("non-mutation query CTE command")),
        })
    }
}
