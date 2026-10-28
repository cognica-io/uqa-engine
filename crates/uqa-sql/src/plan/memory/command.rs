//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use uqa_core::memory::{AllocationObserver, ObserveAllocations};
use uqa_core::ValueRetentionError;

use crate::plan::{
    AssignmentPlan, ConflictActionPlan, ConflictPlan, DeletePlan, InsertPlan, MergePlan,
    MergeWhenPlan, RuleValuesInput, UpdatePlan, ViewCheckPlan, ViewRuleInsertPlan,
    ViewRuleReturningPlan, ViewRuleUpdatePlan,
};

impl ObserveAllocations for InsertPlan {
    fn observe_allocations<'a>(
        &'a self,
        observer: &mut AllocationObserver<'a, '_>,
    ) -> Result<(), ValueRetentionError> {
        let Self {
            table,
            target_relation_bound: _,
            relations_bound: _,
            statement_privilege_subject,
            target_privilege_subject,
            target_qualifier,
            target_alias,
            include_descendants: _,
            columns,
            overriding: _,
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
        } = self;
        observer.child(table)?;
        observer.child(statement_privilege_subject)?;
        observer.child(target_privilege_subject)?;
        observer.child(target_qualifier)?;
        observer.child(target_alias)?;
        observer.child(columns)?;
        observer.child(ctes)?;
        observer.child(rows)?;
        observer.child(source)?;
        observer.child(rule_values_input)?;
        observer.child(on_conflict)?;
        observer.child(returning)?;
        observer.child(returning_aliases)?;
        observer.child(subqueries)?;
        observer.child(view_checks)?;
        observer.child(view_rule_relations)?;
        observer.child(view_rule_insert_plans)?;
        observer.child(view_rule_returning)?;
        Ok(())
    }
}

impl ObserveAllocations for UpdatePlan {
    fn observe_allocations<'a>(
        &'a self,
        observer: &mut AllocationObserver<'a, '_>,
    ) -> Result<(), ValueRetentionError> {
        let Self {
            table,
            target_relation_bound: _,
            relations_bound: _,
            statement_privilege_subject,
            target_privilege_subject,
            target_qualifier,
            target_alias,
            include_descendants: _,
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
        } = self;
        observer.child(table)?;
        observer.child(statement_privilege_subject)?;
        observer.child(target_privilege_subject)?;
        observer.child(target_qualifier)?;
        observer.child(target_alias)?;
        observer.child(assignments)?;
        observer.child(predicate)?;
        observer.child(ctes)?;
        observer.child(source)?;
        observer.child(returning)?;
        observer.child(returning_aliases)?;
        observer.child(subqueries)?;
        observer.child(view_checks)?;
        observer.child(view_rule_relations)?;
        observer.child(view_rule_update_plans)?;
        observer.child(view_rule_returning)?;
        Ok(())
    }
}

impl ObserveAllocations for DeletePlan {
    fn observe_allocations<'a>(
        &'a self,
        observer: &mut AllocationObserver<'a, '_>,
    ) -> Result<(), ValueRetentionError> {
        let Self {
            table,
            target_relation_bound: _,
            relations_bound: _,
            statement_privilege_subject,
            target_privilege_subject,
            target_qualifier,
            target_alias,
            include_descendants: _,
            predicate,
            ctes,
            source,
            returning,
            returning_aliases,
            subqueries,
            view_rule_relations,
            view_rule_returning,
        } = self;
        observer.child(table)?;
        observer.child(statement_privilege_subject)?;
        observer.child(target_privilege_subject)?;
        observer.child(target_qualifier)?;
        observer.child(target_alias)?;
        observer.child(predicate)?;
        observer.child(ctes)?;
        observer.child(source)?;
        observer.child(returning)?;
        observer.child(returning_aliases)?;
        observer.child(subqueries)?;
        observer.child(view_rule_relations)?;
        observer.child(view_rule_returning)?;
        Ok(())
    }
}

impl ObserveAllocations for MergePlan {
    fn observe_allocations<'a>(
        &'a self,
        observer: &mut AllocationObserver<'a, '_>,
    ) -> Result<(), ValueRetentionError> {
        let Self {
            ctes,
            target,
            statement_privilege_subject,
            target_privilege_subject,
            target_qualifier,
            target_alias,
            include_descendants: _,
            target_predicate,
            source,
            join_condition,
            when_clauses,
            returning,
            returning_aliases,
            subqueries,
            view_checks,
        } = self;
        observer.child(ctes)?;
        observer.child(target)?;
        observer.child(statement_privilege_subject)?;
        observer.child(target_privilege_subject)?;
        observer.child(target_qualifier)?;
        observer.child(target_alias)?;
        observer.child(target_predicate)?;
        observer.child(source)?;
        observer.child(join_condition)?;
        observer.child(when_clauses)?;
        observer.child(returning)?;
        observer.child(returning_aliases)?;
        observer.child(subqueries)?;
        observer.child(view_checks)?;
        Ok(())
    }
}

impl ObserveAllocations for AssignmentPlan {
    fn observe_allocations<'a>(
        &'a self,
        observer: &mut AllocationObserver<'a, '_>,
    ) -> Result<(), ValueRetentionError> {
        let Self { target, value } = self;
        observer.child(target)?;
        observer.child(value)?;
        Ok(())
    }
}

impl ObserveAllocations for ConflictPlan {
    fn observe_allocations<'a>(
        &'a self,
        observer: &mut AllocationObserver<'a, '_>,
    ) -> Result<(), ValueRetentionError> {
        let Self {
            predicate,
            constraint,
            conflict_columns,
            expressions,
            action,
        } = self;
        observer.child(predicate)?;
        observer.child(constraint)?;
        observer.child(conflict_columns)?;
        observer.child(expressions)?;
        observer.child(action)?;
        Ok(())
    }
}

impl ObserveAllocations for ConflictActionPlan {
    fn observe_allocations<'a>(
        &'a self,
        observer: &mut AllocationObserver<'a, '_>,
    ) -> Result<(), ValueRetentionError> {
        match self {
            Self::Nothing => {}
            Self::Update {
                assignments,
                predicate,
            } => {
                observer.child(assignments)?;
                observer.child(predicate)?;
            }
        }
        Ok(())
    }
}

impl ObserveAllocations for MergeWhenPlan {
    fn observe_allocations<'a>(
        &'a self,
        observer: &mut AllocationObserver<'a, '_>,
    ) -> Result<(), ValueRetentionError> {
        match self {
            Self::UpdateMatched {
                condition,
                assignments,
            } => {
                observer.child(condition)?;
                observer.child(assignments)?;
            }
            Self::DeleteMatched { condition } => {
                observer.child(condition)?;
            }
            Self::UpdateNotMatchedBySource {
                condition,
                assignments,
            } => {
                observer.child(condition)?;
                observer.child(assignments)?;
            }
            Self::DeleteNotMatchedBySource { condition } => {
                observer.child(condition)?;
            }
            Self::InsertNotMatched {
                condition,
                columns,
                overriding: _,
                values,
            } => {
                observer.child(condition)?;
                observer.child(columns)?;
                observer.child(values)?;
            }
            Self::NothingMatched { condition } => {
                observer.child(condition)?;
            }
            Self::NothingNotMatched { condition } => {
                observer.child(condition)?;
            }
            Self::NothingNotMatchedBySource { condition } => {
                observer.child(condition)?;
            }
        }
        Ok(())
    }
}

impl ObserveAllocations for RuleValuesInput {
    fn observe_allocations<'a>(
        &'a self,
        observer: &mut AllocationObserver<'a, '_>,
    ) -> Result<(), ValueRetentionError> {
        let Self {
            width: _,
            inline: _,
            defaults,
            unresolved,
            written,
        } = self;
        observer.child(defaults)?;
        observer.child(unresolved)?;
        observer.child(written)?;
        Ok(())
    }
}

impl ObserveAllocations for ViewCheckPlan {
    fn observe_allocations<'a>(
        &'a self,
        observer: &mut AllocationObserver<'a, '_>,
    ) -> Result<(), ValueRetentionError> {
        let Self { view, predicate } = self;
        observer.child(view)?;
        observer.child(predicate)?;
        Ok(())
    }
}

impl ObserveAllocations for ViewRuleInsertPlan {
    fn observe_allocations<'a>(
        &'a self,
        observer: &mut AllocationObserver<'a, '_>,
    ) -> Result<(), ValueRetentionError> {
        let Self {
            relation,
            supplied_columns,
            input_columns,
        } = self;
        observer.child(relation)?;
        observer.child(supplied_columns)?;
        observer.child(input_columns)?;
        Ok(())
    }
}

impl ObserveAllocations for ViewRuleUpdatePlan {
    fn observe_allocations<'a>(
        &'a self,
        observer: &mut AllocationObserver<'a, '_>,
    ) -> Result<(), ValueRetentionError> {
        let Self {
            relation,
            assigned_columns,
            input_columns,
        } = self;
        observer.child(relation)?;
        observer.child(assigned_columns)?;
        observer.child(input_columns)?;
        Ok(())
    }
}

impl ObserveAllocations for ViewRuleReturningPlan {
    fn observe_allocations<'a>(
        &'a self,
        observer: &mut AllocationObserver<'a, '_>,
    ) -> Result<(), ValueRetentionError> {
        let Self {
            relation,
            target_qualifier,
            returning,
            aliases,
            subqueries,
        } = self;
        observer.child(relation)?;
        observer.child(target_qualifier)?;
        observer.child(returning)?;
        observer.child(aliases)?;
        observer.child(subqueries)?;
        Ok(())
    }
}
