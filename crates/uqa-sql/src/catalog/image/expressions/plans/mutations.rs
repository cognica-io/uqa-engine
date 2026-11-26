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

projected_struct!(
    recursive,
    InsertPlan {
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
    } skip { prepared_domain_defaults }
);
projected_struct!(
    recursive,
    UpdatePlan {
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
    } skip { prepared_domain_defaults }
);
projected_struct!(
    recursive,
    DeletePlan {
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
    }
);
projected_struct!(
    recursive,
    MergePlan {
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
    } skip { prepared_domain_defaults }
);
projected_struct!(
    inline,
    RuleValuesInput {
        width,
        inline,
        defaults,
        unresolved,
        written
    }
);
projected_struct!(
    inline,
    ConflictPlan {
        predicate,
        constraint,
        conflict_columns,
        expressions,
        action
    }
);
projected_struct!(inline, AssignmentPlan { target, value });
projected_struct!(inline, AssignmentTarget<ScalarExpr> { column, indirection });
projected_struct!(inline, MultipleAssignmentTargets<ScalarExpr> { targets, source_positions, source_width });
projected_struct!(inline, ViewCheckPlan { view, predicate });
projected_struct!(
    inline,
    ViewRuleReturningPlan {
        relation,
        target_qualifier,
        returning,
        aliases,
        subqueries
    }
);
projected_struct!(
    inline,
    ViewRuleInsertPlan {
        relation,
        supplied_columns,
        input_columns
    }
);
projected_struct!(
    inline,
    ViewRuleUpdatePlan {
        relation,
        assigned_columns,
        input_columns
    }
);
projected_enum!(inline, AssignmentTargets<ScalarExpr> { 0 => Single(value), 1 => Multiple(value) });
projected_enum!(inline, AssignmentStep<ScalarExpr> { 0 => Field(value), 1 => Index(value), 2 => Slice { lower, upper } });
projected_enum!(inline, ConflictActionPlan { 0 => Nothing, 1 => Update { assignments, predicate } });
projected_enum!(inline, MergeWhenPlan {
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
