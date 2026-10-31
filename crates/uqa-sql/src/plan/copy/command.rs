//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{Copying, Result};
use crate::plan::{
    CommandPlan, DeletePlan, MergePlan, RuleValuesInput, UpdatePlan, ViewCheckPlan,
    ViewRuleReturningPlan, ViewRuleUpdatePlan,
};

mod assignments;
mod insert;

impl Copying<'_> {
    pub(in crate::plan) fn insert_command(
        &mut self,
        plan: &crate::plan::InsertPlan,
    ) -> Result<CommandPlan> {
        self.check()?;
        Ok(CommandPlan::Insert(self.field("value0", |copying| {
            copying.boxed(|copying| copying.insert(plan))
        })?))
    }

    pub(in crate::plan) fn update_command(
        &mut self,
        plan: &crate::plan::UpdatePlan,
    ) -> Result<CommandPlan> {
        self.check()?;
        Ok(CommandPlan::Update(self.field("value0", |copying| {
            copying.boxed(|copying| copying.update(plan))
        })?))
    }

    pub(in crate::plan) fn delete_command(
        &mut self,
        plan: &crate::plan::DeletePlan,
    ) -> Result<CommandPlan> {
        self.check()?;
        Ok(CommandPlan::Delete(self.field("value0", |copying| {
            copying.boxed(|copying| copying.delete(plan))
        })?))
    }

    pub(in crate::plan) fn merge_command(
        &mut self,
        plan: &crate::plan::MergePlan,
    ) -> Result<CommandPlan> {
        self.check()?;
        Ok(CommandPlan::Merge(self.field("value0", |copying| {
            copying.boxed(|copying| copying.merge(plan))
        })?))
    }

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

    pub(super) fn update(&mut self, plan: &UpdatePlan) -> Result<UpdatePlan> {
        Ok(UpdatePlan {
            table: self.field("table", |copying| copying.text(&plan.table))?,
            target_relation_bound: plan.target_relation_bound,
            relations_bound: plan.relations_bound,
            statement_privilege_subject: self.role(plan.statement_privilege_subject.as_ref())?,
            target_privilege_subject: self.field("target_privilege_subject", |copying| {
                copying.role(plan.target_privilege_subject.as_ref())
            })?,
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
            view_checks: self.field("view_checks", |copying| {
                copying.map(plan.view_checks.iter(), Self::view_check)
            })?,
            view_rule_relations: self.field("view_rule_relations", |copying| {
                copying.texts(&plan.view_rule_relations)
            })?,
            view_rule_update_plans: self.field("view_rule_update_plans", |copying| {
                copying.map(plan.view_rule_update_plans.iter(), |copying, input| {
                    Ok(ViewRuleUpdatePlan {
                        relation: copying.text(&input.relation)?,
                        assigned_columns: copying.field("assigned_columns", |copying| {
                            copying.texts(&input.assigned_columns)
                        })?,
                        input_columns: copying.field("input_columns", |copying| {
                            copying.texts(&input.input_columns)
                        })?,
                    })
                })
            })?,
            view_rule_returning: self.field("view_rule_returning", |copying| {
                plan.view_rule_returning
                    .as_ref()
                    .map(|returning| copying.rule_returning(returning))
                    .transpose()
            })?,
        })
    }

    pub(super) fn delete(&mut self, plan: &DeletePlan) -> Result<DeletePlan> {
        Ok(DeletePlan {
            table: self.field("table", |copying| copying.text(&plan.table))?,
            target_relation_bound: plan.target_relation_bound,
            relations_bound: plan.relations_bound,
            statement_privilege_subject: self.role(plan.statement_privilege_subject.as_ref())?,
            target_privilege_subject: self.field("target_privilege_subject", |copying| {
                copying.role(plan.target_privilege_subject.as_ref())
            })?,
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
            view_rule_relations: self.field("view_rule_relations", |copying| {
                copying.texts(&plan.view_rule_relations)
            })?,
            view_rule_returning: self.field("view_rule_returning", |copying| {
                plan.view_rule_returning
                    .as_ref()
                    .map(|returning| copying.rule_returning(returning))
                    .transpose()
            })?,
        })
    }

    pub(super) fn merge(&mut self, plan: &MergePlan) -> Result<MergePlan> {
        Ok(MergePlan {
            ctes: self.field("ctes", |copying| copying.map(plan.ctes.iter(), Self::cte))?,
            target: self.field("target", |copying| copying.text(&plan.target))?,
            statement_privilege_subject: self.role(plan.statement_privilege_subject.as_ref())?,
            target_privilege_subject: self.field("target_privilege_subject", |copying| {
                copying.role(plan.target_privilege_subject.as_ref())
            })?,
            target_qualifier: self.text(&plan.target_qualifier)?,
            target_alias: self.optional_text(plan.target_alias.as_deref())?,
            include_descendants: plan.include_descendants,
            target_predicate: self.field("target_predicate", |copying| {
                copying.optional_scalar(plan.target_predicate.as_ref())
            })?,
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
            view_checks: self.field("view_checks", |copying| {
                copying.map(plan.view_checks.iter(), Self::view_check)
            })?,
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

    pub(super) fn returning_aliases(
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
            predicate: self.field("predicate", |copying| copying.scalar(&check.predicate))?,
        })
    }

    fn rule_returning(
        &mut self,
        returning: &ViewRuleReturningPlan,
    ) -> Result<ViewRuleReturningPlan> {
        self.rule_returning_inputs(
            &returning.relation,
            &returning.target_qualifier,
            &returning.returning,
            &returning.aliases,
            &returning.subqueries,
        )
    }

    pub(in crate::plan) fn rule_returning_inputs(
        &mut self,
        relation: &str,
        qualifier: &str,
        projections: &[crate::plan::ProjectionPlan],
        aliases: &crate::ast::ReturningAliases,
        queries: &[crate::plan::QueryPlan],
    ) -> Result<ViewRuleReturningPlan> {
        Ok(ViewRuleReturningPlan {
            relation: self.text(relation)?,
            target_qualifier: self.text(qualifier)?,
            returning: self.map(projections.iter(), Self::projection)?,
            aliases: self.returning_aliases(aliases)?,
            subqueries: self.map(queries.iter(), Self::query)?,
        })
    }
}
