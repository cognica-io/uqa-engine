//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{Copying, Result};
use crate::plan::{
    CommandPlan, DeletePlan, InsertPlan, MergePlan, RuleValuesInput, UpdatePlan, ViewCheckPlan,
    ViewRuleInsertPlan, ViewRuleReturningPlan, ViewRuleUpdatePlan,
};

mod assignments;

impl Copying<'_> {
    pub(super) fn command(&mut self, command: &CommandPlan) -> Result<CommandPlan> {
        self.check()?;
        super::require_mutation(command)?;
        Ok(match command {
            CommandPlan::Insert(plan) => {
                CommandPlan::Insert(self.boxed(|copying| copying.insert(plan))?)
            }
            CommandPlan::Update(plan) => {
                CommandPlan::Update(self.boxed(|copying| copying.update(plan))?)
            }
            CommandPlan::Delete(plan) => {
                CommandPlan::Delete(self.boxed(|copying| copying.delete(plan))?)
            }
            CommandPlan::Merge(plan) => {
                CommandPlan::Merge(self.boxed(|copying| copying.merge(plan))?)
            }
            _ => unreachable!("mutation command selected above"),
        })
    }

    pub(super) fn insert(&mut self, plan: &InsertPlan) -> Result<InsertPlan> {
        Ok(InsertPlan {
            table: self.text(&plan.table)?,
            target_relation_bound: plan.target_relation_bound,
            relations_bound: plan.relations_bound,
            statement_privilege_subject: self.role(plan.statement_privilege_subject.as_ref())?,
            target_privilege_subject: self.role(plan.target_privilege_subject.as_ref())?,
            target_qualifier: self.text(&plan.target_qualifier)?,
            target_alias: self.optional_text(plan.target_alias.as_deref())?,
            include_descendants: plan.include_descendants,
            columns: self.map(plan.columns.iter(), Self::assignment_target)?,
            overriding: plan.overriding,
            ctes: self.field("ctes", |copying| copying.map(plan.ctes.iter(), Self::cte))?,
            rows: self.field("rows", |copying| copying.rows(&plan.rows))?,
            source: self.field("source", |copying| {
                plan.source
                    .as_ref()
                    .map(|source| copying.boxed(|copying| copying.query(source)))
                    .transpose()
            })?,
            rule_values_input: plan
                .rule_values_input
                .as_ref()
                .map(|input| self.rule_values(input))
                .transpose()?,
            on_conflict: self.field("on_conflict", |copying| {
                plan.on_conflict
                    .as_ref()
                    .map(|conflict| copying.conflict(conflict))
                    .transpose()
            })?,
            returning: self.field("returning", |copying| {
                copying.map(plan.returning.iter(), Self::projection)
            })?,
            returning_aliases: self.returning_aliases(&plan.returning_aliases)?,
            subqueries: self.field("subqueries", |copying| {
                copying.map(plan.subqueries.iter(), Self::query)
            })?,
            view_checks: self.map(plan.view_checks.iter(), Self::view_check)?,
            view_rule_relations: self.texts(&plan.view_rule_relations)?,
            view_rule_insert_plans: self.map(
                plan.view_rule_insert_plans.iter(),
                |copying, input| {
                    Ok(ViewRuleInsertPlan {
                        relation: copying.text(&input.relation)?,
                        supplied_columns: copying.texts(&input.supplied_columns)?,
                        input_columns: copying.texts(&input.input_columns)?,
                    })
                },
            )?,
            view_rule_returning: plan
                .view_rule_returning
                .as_ref()
                .map(|returning| self.rule_returning(returning))
                .transpose()?,
        })
    }

    pub(super) fn update(&mut self, plan: &UpdatePlan) -> Result<UpdatePlan> {
        Ok(UpdatePlan {
            table: self.text(&plan.table)?,
            target_relation_bound: plan.target_relation_bound,
            relations_bound: plan.relations_bound,
            statement_privilege_subject: self.role(plan.statement_privilege_subject.as_ref())?,
            target_privilege_subject: self.role(plan.target_privilege_subject.as_ref())?,
            target_qualifier: self.text(&plan.target_qualifier)?,
            target_alias: self.optional_text(plan.target_alias.as_deref())?,
            include_descendants: plan.include_descendants,
            assignments: self.field("assignments", |copying| {
                copying.map(plan.assignments.iter(), Self::assignment)
            })?,
            predicate: self.field("predicate", |copying| {
                copying.optional_scalar(plan.predicate.as_ref())
            })?,
            ctes: self.field("ctes", |copying| copying.map(plan.ctes.iter(), Self::cte))?,
            source: self.field("source", |copying| {
                plan.source
                    .as_ref()
                    .map(|source| copying.boxed(|copying| copying.source(source)))
                    .transpose()
            })?,
            returning: self.field("returning", |copying| {
                copying.map(plan.returning.iter(), Self::projection)
            })?,
            returning_aliases: self.returning_aliases(&plan.returning_aliases)?,
            subqueries: self.field("subqueries", |copying| {
                copying.map(plan.subqueries.iter(), Self::query)
            })?,
            view_checks: self.map(plan.view_checks.iter(), Self::view_check)?,
            view_rule_relations: self.texts(&plan.view_rule_relations)?,
            view_rule_update_plans: self.map(
                plan.view_rule_update_plans.iter(),
                |copying, input| {
                    Ok(ViewRuleUpdatePlan {
                        relation: copying.text(&input.relation)?,
                        assigned_columns: copying.texts(&input.assigned_columns)?,
                        input_columns: copying.texts(&input.input_columns)?,
                    })
                },
            )?,
            view_rule_returning: plan
                .view_rule_returning
                .as_ref()
                .map(|returning| self.rule_returning(returning))
                .transpose()?,
        })
    }

    pub(super) fn delete(&mut self, plan: &DeletePlan) -> Result<DeletePlan> {
        Ok(DeletePlan {
            table: self.text(&plan.table)?,
            target_relation_bound: plan.target_relation_bound,
            relations_bound: plan.relations_bound,
            statement_privilege_subject: self.role(plan.statement_privilege_subject.as_ref())?,
            target_privilege_subject: self.role(plan.target_privilege_subject.as_ref())?,
            target_qualifier: self.text(&plan.target_qualifier)?,
            target_alias: self.optional_text(plan.target_alias.as_deref())?,
            include_descendants: plan.include_descendants,
            predicate: self.field("predicate", |copying| {
                copying.optional_scalar(plan.predicate.as_ref())
            })?,
            ctes: self.field("ctes", |copying| copying.map(plan.ctes.iter(), Self::cte))?,
            source: self.field("source", |copying| {
                plan.source
                    .as_ref()
                    .map(|source| copying.boxed(|copying| copying.source(source)))
                    .transpose()
            })?,
            returning: self.field("returning", |copying| {
                copying.map(plan.returning.iter(), Self::projection)
            })?,
            returning_aliases: self.returning_aliases(&plan.returning_aliases)?,
            subqueries: self.field("subqueries", |copying| {
                copying.map(plan.subqueries.iter(), Self::query)
            })?,
            view_rule_relations: self.texts(&plan.view_rule_relations)?,
            view_rule_returning: plan
                .view_rule_returning
                .as_ref()
                .map(|returning| self.rule_returning(returning))
                .transpose()?,
        })
    }

    pub(super) fn merge(&mut self, plan: &MergePlan) -> Result<MergePlan> {
        Ok(MergePlan {
            ctes: self.field("ctes", |copying| copying.map(plan.ctes.iter(), Self::cte))?,
            target: self.text(&plan.target)?,
            statement_privilege_subject: self.role(plan.statement_privilege_subject.as_ref())?,
            target_privilege_subject: self.role(plan.target_privilege_subject.as_ref())?,
            target_qualifier: self.text(&plan.target_qualifier)?,
            target_alias: self.optional_text(plan.target_alias.as_deref())?,
            include_descendants: plan.include_descendants,
            target_predicate: self.optional_scalar(plan.target_predicate.as_ref())?,
            source: self.field("source", |copying| {
                copying.boxed(|copying| copying.source(&plan.source))
            })?,
            join_condition: self.field("join_condition", |copying| {
                copying.scalar(&plan.join_condition)
            })?,
            when_clauses: self.field("when_clauses", |copying| {
                copying.map(plan.when_clauses.iter(), Self::merge_when)
            })?,
            returning: self.field("returning", |copying| {
                copying.map(plan.returning.iter(), Self::projection)
            })?,
            returning_aliases: self.returning_aliases(&plan.returning_aliases)?,
            subqueries: self.field("subqueries", |copying| {
                copying.map(plan.subqueries.iter(), Self::query)
            })?,
            view_checks: self.map(plan.view_checks.iter(), Self::view_check)?,
        })
    }

    fn rule_values(&mut self, input: &RuleValuesInput) -> Result<RuleValuesInput> {
        Ok(RuleValuesInput {
            width: input.width,
            inline: input.inline,
            defaults: self.map(input.defaults.iter(), |_, position| Ok(*position))?,
            unresolved: self.set(&input.unresolved, |_, position| Ok(*position))?,
            written: self.set(&input.written, |_, position| Ok(*position))?,
        })
    }

    fn returning_aliases(
        &mut self,
        aliases: &crate::ast::ReturningAliases,
    ) -> Result<crate::ast::ReturningAliases> {
        Ok(crate::ast::ReturningAliases {
            old: self.text(&aliases.old)?,
            new: self.text(&aliases.new)?,
            old_explicit: aliases.old_explicit,
            new_explicit: aliases.new_explicit,
        })
    }

    fn view_check(&mut self, check: &ViewCheckPlan) -> Result<ViewCheckPlan> {
        Ok(ViewCheckPlan {
            view: self.text(&check.view)?,
            predicate: self.scalar(&check.predicate)?,
        })
    }

    fn rule_returning(
        &mut self,
        returning: &ViewRuleReturningPlan,
    ) -> Result<ViewRuleReturningPlan> {
        Ok(ViewRuleReturningPlan {
            relation: self.text(&returning.relation)?,
            target_qualifier: self.text(&returning.target_qualifier)?,
            returning: self.map(returning.returning.iter(), Self::projection)?,
            aliases: self.returning_aliases(&returning.aliases)?,
            subqueries: self.map(returning.subqueries.iter(), Self::query)?,
        })
    }
}
