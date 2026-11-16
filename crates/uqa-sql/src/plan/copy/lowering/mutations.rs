//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{Copying, Result};
use crate::{
    ast::{DeleteStmt, InsertStmt, MergeStmt, UpdateStmt},
    plan::{
        retirement::Pending, AggregateClassifier, DeletePlan, InsertPlan, MergePlan, UpdatePlan,
    },
};

mod assignments;

impl Copying<'_> {
    pub(super) fn lower_insert(
        &mut self,
        statement: &InsertStmt,
        aggregates: &dyn AggregateClassifier,
    ) -> Result<InsertPlan> {
        let mut scope = self.enter_plan_node()?;
        scope.lower_insert_fields(statement, aggregates)
    }

    fn lower_insert_fields(
        &mut self,
        statement: &InsertStmt,
        aggregates: &dyn AggregateClassifier,
    ) -> Result<InsertPlan> {
        let ctes = self.field("ctes", |copying| {
            copying.lower_ctes(&statement.with, aggregates)
        })?;
        let ctes = self.retain_plan(ctes);
        let source = self.field("source", |copying| {
            statement
                .select_source
                .as_ref()
                .map(|query| {
                    copying.lower_boxed(|copying| copying.lower_nested_query(query, aggregates))
                })
                .transpose()
        })?;
        let source = self.retain_plan(source);
        let mut arena = self.query_arena();
        let rows = self.field("rows", |copying| {
            copying.lower_rows(&statement.rows, aggregates, &mut arena)
        })?;
        let rows = Pending::new(rows);
        let on_conflict = self.field("on_conflict", |copying| {
            statement
                .on_conflict
                .as_ref()
                .map(|conflict| copying.lower_conflict(conflict, aggregates, &mut arena))
                .transpose()
        })?;
        let on_conflict = Pending::new(on_conflict);
        let returning = self.field("returning", |copying| {
            copying.lower_projections(&statement.returning, aggregates, &mut arena)
        })?;
        let returning = Pending::new(returning);
        let table = self.field("table", |copying| copying.text(&statement.table))?;
        let target_privilege_subject = self.field("target_privilege_subject", |_| Ok(None))?;
        let target_qualifier = self.text(&statement.target_qualifier)?;
        let target_alias = self.optional_text(statement.target_alias.as_deref())?;
        let columns = self.field("columns", |copying| {
            copying.lower_assignment_targets(&statement.columns, aggregates, &mut arena)
        })?;
        let columns = Pending::new(columns);
        let rule_values_input = self.field("rule_values_input", |_| Ok(None))?;
        let returning_aliases = self.returning_aliases(&statement.returning_aliases)?;
        let subqueries = self.take_arena(arena)?;
        let subqueries = self.retain_plan(subqueries);
        let view_checks = self.field("view_checks", |_| Ok(Vec::new()))?;
        let view_checks = Pending::new(view_checks);
        let view_rule_relations = self.field("view_rule_relations", |_| Ok(Vec::new()))?;
        let view_rule_insert_plans = self.field("view_rule_insert_plans", |_| Ok(Vec::new()))?;
        let view_rule_returning = self.field("view_rule_returning", |_| Ok(None))?;
        Ok(InsertPlan {
            table,
            target_relation_bound: statement.target_relation_bound,
            relations_bound: false,
            statement_privilege_subject: None,
            target_privilege_subject,
            target_qualifier,
            target_alias,
            include_descendants: statement.include_descendants,
            columns: columns.take(),
            overriding: statement.overriding,
            ctes: ctes.take(),
            rows: rows.take(),
            source: source.take(),
            rule_values_input,
            on_conflict: on_conflict.take(),
            returning: returning.take(),
            returning_aliases,
            subqueries: subqueries.take(),
            view_checks: view_checks.take(),
            view_rule_relations,
            view_rule_insert_plans,
            view_rule_returning,
        })
    }

    pub(super) fn lower_update(
        &mut self,
        statement: &UpdateStmt,
        aggregates: &dyn AggregateClassifier,
    ) -> Result<UpdatePlan> {
        let mut scope = self.enter_plan_node()?;
        scope.lower_update_fields(statement, aggregates)
    }

    fn lower_update_fields(
        &mut self,
        statement: &UpdateStmt,
        aggregates: &dyn AggregateClassifier,
    ) -> Result<UpdatePlan> {
        let ctes = self.field("ctes", |copying| {
            copying.lower_ctes(&statement.with, aggregates)
        })?;
        let ctes = self.retain_plan(ctes);
        let mut arena = self.query_arena();
        let source = self.field("source", |copying| {
            statement
                .from
                .as_ref()
                .map(|source| {
                    copying
                        .lower_boxed(|copying| copying.lower_source(source, aggregates, &mut arena))
                })
                .transpose()
        })?;
        let source = self.retain_plan(source);
        let assignments = self.field("assignments", |copying| {
            copying.lower_assignments(&statement.assignments, aggregates, &mut arena)
        })?;
        let assignments = Pending::new(assignments);
        let predicate = self.field("predicate", |copying| {
            copying.lower_optional_scalar(statement.r#where.as_ref(), aggregates, &mut arena)
        })?;
        let predicate = Pending::new(predicate);
        let returning = self.field("returning", |copying| {
            copying.lower_projections(&statement.returning, aggregates, &mut arena)
        })?;
        let returning = Pending::new(returning);
        let table = self.field("table", |copying| copying.text(&statement.table))?;
        let target_privilege_subject = self.field("target_privilege_subject", |_| Ok(None))?;
        let target_qualifier = self.text(&statement.target_qualifier)?;
        let target_alias = self.optional_text(statement.target_alias.as_deref())?;
        let returning_aliases = self.returning_aliases(&statement.returning_aliases)?;
        let subqueries = self.take_arena(arena)?;
        let subqueries = self.retain_plan(subqueries);
        let view_checks = self.field("view_checks", |_| Ok(Vec::new()))?;
        let view_checks = Pending::new(view_checks);
        let view_rule_relations = self.field("view_rule_relations", |_| Ok(Vec::new()))?;
        let view_rule_update_plans = self.field("view_rule_update_plans", |_| Ok(Vec::new()))?;
        let view_rule_returning = self.field("view_rule_returning", |_| Ok(None))?;
        Ok(UpdatePlan {
            table,
            target_relation_bound: statement.target_relation_bound,
            relations_bound: false,
            statement_privilege_subject: None,
            target_privilege_subject,
            target_qualifier,
            target_alias,
            include_descendants: statement.include_descendants,
            assignments: assignments.take(),
            predicate: predicate.take(),
            ctes: ctes.take(),
            source: source.take(),
            returning: returning.take(),
            returning_aliases,
            subqueries: subqueries.take(),
            view_checks: view_checks.take(),
            view_rule_relations,
            view_rule_update_plans,
            view_rule_returning,
        })
    }

    pub(super) fn lower_delete(
        &mut self,
        statement: &DeleteStmt,
        aggregates: &dyn AggregateClassifier,
    ) -> Result<DeletePlan> {
        let mut scope = self.enter_plan_node()?;
        scope.lower_delete_fields(statement, aggregates)
    }

    fn lower_delete_fields(
        &mut self,
        statement: &DeleteStmt,
        aggregates: &dyn AggregateClassifier,
    ) -> Result<DeletePlan> {
        let ctes = self.field("ctes", |copying| {
            copying.lower_ctes(&statement.with, aggregates)
        })?;
        let ctes = self.retain_plan(ctes);
        let mut arena = self.query_arena();
        let source = self.field("source", |copying| {
            statement
                .using
                .as_ref()
                .map(|source| {
                    copying
                        .lower_boxed(|copying| copying.lower_source(source, aggregates, &mut arena))
                })
                .transpose()
        })?;
        let source = self.retain_plan(source);
        let predicate = self.field("predicate", |copying| {
            copying.lower_optional_scalar(statement.r#where.as_ref(), aggregates, &mut arena)
        })?;
        let predicate = Pending::new(predicate);
        let returning = self.field("returning", |copying| {
            copying.lower_projections(&statement.returning, aggregates, &mut arena)
        })?;
        let returning = Pending::new(returning);
        let table = self.field("table", |copying| copying.text(&statement.table))?;
        let target_privilege_subject = self.field("target_privilege_subject", |_| Ok(None))?;
        let target_qualifier = self.text(&statement.target_qualifier)?;
        let target_alias = self.optional_text(statement.target_alias.as_deref())?;
        let returning_aliases = self.returning_aliases(&statement.returning_aliases)?;
        let subqueries = self.take_arena(arena)?;
        let subqueries = self.retain_plan(subqueries);
        let view_rule_relations = self.field("view_rule_relations", |_| Ok(Vec::new()))?;
        let view_rule_returning = self.field("view_rule_returning", |_| Ok(None))?;
        Ok(DeletePlan {
            table,
            target_relation_bound: statement.target_relation_bound,
            relations_bound: false,
            statement_privilege_subject: None,
            target_privilege_subject,
            target_qualifier,
            target_alias,
            include_descendants: statement.include_descendants,
            predicate: predicate.take(),
            ctes: ctes.take(),
            source: source.take(),
            returning: returning.take(),
            returning_aliases,
            subqueries: subqueries.take(),
            view_rule_relations,
            view_rule_returning,
        })
    }

    pub(super) fn lower_merge(
        &mut self,
        statement: &MergeStmt,
        aggregates: &dyn AggregateClassifier,
    ) -> Result<MergePlan> {
        let mut scope = self.enter_plan_node()?;
        scope.lower_merge_fields(statement, aggregates)
    }

    fn lower_merge_fields(
        &mut self,
        statement: &MergeStmt,
        aggregates: &dyn AggregateClassifier,
    ) -> Result<MergePlan> {
        let mut arena = self.query_arena();
        let source = self.field("source", |copying| {
            copying.lower_boxed(|copying| {
                copying.lower_source(&statement.source, aggregates, &mut arena)
            })
        })?;
        let source = self.retain_plan(source);
        let join_condition = self.field("join_condition", |copying| {
            copying.lower_scalar(&statement.join_condition, aggregates, &mut arena)
        })?;
        let join_condition = Pending::new(join_condition);
        let when_clauses = self.field("when_clauses", |copying| {
            copying.lower_merge_whens(&statement.when_clauses, aggregates, &mut arena)
        })?;
        let when_clauses = Pending::new(when_clauses);
        let returning = self.field("returning", |copying| {
            copying.lower_projections(&statement.returning, aggregates, &mut arena)
        })?;
        let returning = Pending::new(returning);
        let ctes = self.field("ctes", |copying| {
            copying.lower_ctes(&statement.with, aggregates)
        })?;
        let ctes = self.retain_plan(ctes);
        let target = self.field("target", |copying| copying.text(&statement.target))?;
        let target_privilege_subject = self.field("target_privilege_subject", |_| Ok(None))?;
        let target_qualifier = self.text(&statement.target_qualifier)?;
        let target_alias = self.optional_text(statement.target_alias.as_deref())?;
        let target_predicate = self.field("target_predicate", |_| Ok(None))?;
        let target_predicate = Pending::new(target_predicate);
        let returning_aliases = self.returning_aliases(&statement.returning_aliases)?;
        let subqueries = self.take_arena(arena)?;
        let subqueries = self.retain_plan(subqueries);
        let view_checks = self.field("view_checks", |_| Ok(Vec::new()))?;
        let view_checks = Pending::new(view_checks);
        Ok(MergePlan {
            ctes: ctes.take(),
            target,
            statement_privilege_subject: None,
            target_privilege_subject,
            target_qualifier,
            target_alias,
            include_descendants: statement.include_descendants,
            target_predicate: target_predicate.take(),
            source: source.take(),
            join_condition: join_condition.take(),
            when_clauses: when_clauses.take(),
            returning: returning.take(),
            returning_aliases,
            subqueries: subqueries.take(),
            view_checks: view_checks.take(),
        })
    }
}
