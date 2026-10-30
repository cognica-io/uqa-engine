//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Select rule-layer fields before admitting a command; discarded sources and view layers are never copied.

use super::{Copying, Result};
use crate::plan::{CommandPlan, InsertPlan, ViewRuleInsertPlan};

impl Copying<'_> {
    pub(in crate::plan::copy) fn insert(&mut self, plan: &InsertPlan) -> Result<InsertPlan> {
        self.selected_insert(plan, None)
    }

    fn selected_insert(
        &mut self,
        plan: &InsertPlan,
        selection: Option<(Option<&ViewRuleInsertPlan>, bool)>,
    ) -> Result<InsertPlan> {
        let selected = InsertFields::select(plan, selection);
        Ok(InsertPlan {
            table: self.text(selected.table)?,
            target_relation_bound: plan.target_relation_bound,
            relations_bound: plan.relations_bound,
            statement_privilege_subject: self.role(plan.statement_privilege_subject.as_ref())?,
            target_privilege_subject: self.role(plan.target_privilege_subject.as_ref())?,
            target_qualifier: self.text(selected.qualifier)?,
            target_alias: self.optional_text(plan.target_alias.as_deref())?,
            include_descendants: plan.include_descendants,
            columns: self.field("columns", |copying| match selected.columns {
                Some(columns) => copying.map(
                    columns.iter().zip(&plan.columns),
                    |copying, (name, target)| copying.assignment_target_named(target, name),
                ),
                None => copying.map(plan.columns.iter(), Self::assignment_target),
            })?,
            overriding: plan.overriding,
            ctes: self.field("ctes", |copying| copying.map(plan.ctes.iter(), Self::cte))?,
            rows: self.field("rows", |copying| copying.rows(selected.rows))?,
            source: self.field("source", |copying| {
                selected
                    .source
                    .map(|source| copying.boxed(|copying| copying.query(source)))
                    .transpose()
            })?,
            rule_values_input: self.field("rule_values_input", |copying| {
                plan.rule_values_input
                    .as_ref()
                    .map(|input| copying.rule_values(input))
                    .transpose()
            })?,
            on_conflict: self.field("on_conflict", |copying| {
                plan.on_conflict
                    .as_ref()
                    .map(|conflict| copying.conflict(conflict))
                    .transpose()
            })?,
            returning: self.field("returning", |copying| {
                copying.map(selected.returning.iter(), Self::projection)
            })?,
            returning_aliases: self.returning_aliases(selected.aliases)?,
            subqueries: self.field("subqueries", |copying| {
                copying.map(selected.subqueries.iter(), Self::query)
            })?,
            view_checks: self.field("view_checks", |copying| {
                copying.map(plan.view_checks.iter(), Self::view_check)
            })?,
            view_rule_relations: self.texts(selected.relations)?,
            view_rule_insert_plans: self.map(selected.layers.iter(), |copying, input| {
                Ok(ViewRuleInsertPlan {
                    relation: copying.text(&input.relation)?,
                    supplied_columns: copying.texts(&input.supplied_columns)?,
                    input_columns: copying.texts(&input.input_columns)?,
                })
            })?,
            view_rule_returning: plan
                .view_rule_returning
                .as_ref()
                .map(|returning| self.rule_returning(returning))
                .transpose()?,
        })
    }

    pub(in crate::plan) fn insert_rule_layer_command(
        &mut self,
        plan: &InsertPlan,
        layer: Option<&ViewRuleInsertPlan>,
        rows: bool,
    ) -> Result<CommandPlan> {
        self.check()?;
        Ok(CommandPlan::Insert(self.field("value0", |copying| {
            copying.boxed(|copying| copying.selected_insert(plan, Some((layer, rows))))
        })?))
    }
}

struct InsertFields<'a> {
    table: &'a str,
    qualifier: &'a str,
    columns: Option<&'a [String]>,
    rows: &'a [Vec<crate::ScalarExpr>],
    source: Option<&'a crate::plan::QueryPlan>,
    returning: &'a [crate::plan::ProjectionPlan],
    aliases: &'a crate::ast::ReturningAliases,
    subqueries: &'a [crate::plan::QueryPlan],
    relations: &'a [String],
    layers: &'a [ViewRuleInsertPlan],
}

impl<'a> InsertFields<'a> {
    fn select(
        plan: &'a InsertPlan,
        selection: Option<(Option<&'a ViewRuleInsertPlan>, bool)>,
    ) -> Self {
        let layer = selection.and_then(|(layer, _)| layer);
        let returning = layer.and_then(|layer| {
            plan.view_rule_returning
                .as_ref()
                .filter(|returning| returning.relation == layer.relation)
        });
        Self {
            table: layer.map_or(plan.table.as_str(), |layer| layer.relation.as_str()),
            qualifier: returning.map_or_else(
                || {
                    layer.map_or(plan.target_qualifier.as_str(), |layer| {
                        layer.relation.as_str()
                    })
                },
                |returning| returning.target_qualifier.as_str(),
            ),
            columns: layer.map(|layer| layer.supplied_columns.as_slice()),
            rows: if selection.is_none_or(|(_, rows)| rows) {
                &plan.rows
            } else {
                &[]
            },
            source: plan.source.as_deref().filter(|_| selection.is_none()),
            returning: returning.map_or_else(
                || {
                    if layer.is_some() {
                        &[][..]
                    } else {
                        plan.returning.as_slice()
                    }
                },
                |returning| returning.returning.as_slice(),
            ),
            aliases: returning.map_or(&plan.returning_aliases, |returning| &returning.aliases),
            subqueries: returning.map_or(plan.subqueries.as_slice(), |returning| {
                returning.subqueries.as_slice()
            }),
            relations: if selection.is_none() {
                &plan.view_rule_relations
            } else {
                &[]
            },
            layers: if selection.is_none() {
                &plan.view_rule_insert_plans
            } else {
                &[]
            },
        }
    }
}
