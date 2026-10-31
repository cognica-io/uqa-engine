//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Prune mutation inputs discarded by rewrite rules before constant evaluation.
use crate::{CommandPlan, RelationalPlan, SourcePlan, UnifiedPlan};
use std::collections::BTreeSet;
use uqa_core::Value;
use uqa_sql::plan::ScopedPlanVisitor;
use uqa_sql::{
    ast::RuleEvent,
    semantics::{rules::RuleCatalog, view_rewrite::context::ViewRewriteContext},
    SQLError, ScalarExpr,
};
mod owned;
mod view_projection;
pub use owned::{prune_captured_inputs_owned, prune_command_inputs_owned};

pub fn rewrite_plan_owned(
    context: &RuleInputPlanningContext<'_>,
    plan: uqa_sql::plan::MutableExecutablePlan,
    config: &crate::optimizer::OptimizerConfig<'_>,
) -> Result<uqa_sql::plan::MutableExecutablePlan, SQLError> {
    let plan = view_projection::rewrite_owned(context, plan, config)?;
    let plan = prune_captured_inputs_owned(
        context.volatility,
        context.views.routines,
        plan,
        &config.production,
    )?;
    prune_command_inputs_owned(context, plan, &config.production)
}

pub trait RuleInputColumns {
    fn source_column_names(
        &self,
        table: &str,
        relations_bound: bool,
    ) -> Result<Option<Vec<String>>, SQLError>;

    fn source_column_names_with_control(
        &self,
        table: &str,
        relations_bound: bool,
        control: &uqa_core::memory::ProductionControl<'_>,
    ) -> Result<Option<uqa_core::memory::Produced<Vec<String>>>, SQLError> {
        control.check()?;
        let Some(columns) = self.source_column_names(table, relations_bound)? else {
            return Ok(None);
        };
        let mut output = uqa_core::memory::ProductionVec::new(*control);
        for column in columns {
            output.push_produced(control.copy_text(&column)?)?;
        }
        Ok(Some(output.finish()?))
    }
}
pub struct RuleInputPlanningContext<'a> {
    pub rules: &'a dyn RuleCatalog,
    pub views: ViewRewriteContext<'a>,
    pub columns: &'a dyn RuleInputColumns,
    pub volatility: &'a dyn uqa_sql::semantics::volatility::VolatilityCatalog,
}
pub fn rewrite_plan(
    context: &RuleInputPlanningContext<'_>,
    plan: &mut UnifiedPlan,
    config: &crate::optimizer::OptimizerConfig<'_>,
) -> Result<(), SQLError> {
    view_projection::rewrite(context, plan, config)?;
    prune_captured_inputs(context.volatility, context.views.routines, plan)?;
    plan.visit_scoped_mut(&mut Rules(context))
}

/// Internal producer slots have structural identities. Their unused projections can be removed before expression folding without changing the query's cardinality or visible namespace.
fn prune_captured_inputs(
    volatility: &dyn uqa_sql::semantics::volatility::VolatilityCatalog,
    routines: &dyn uqa_sql::routines::RoutineResolution,
    plan: &mut UnifiedPlan,
) -> Result<(), SQLError> {
    let mut required = std::collections::BTreeMap::<_, BTreeSet<usize>>::new();
    plan.visit_scalar_expressions(&mut |expression| {
        expression.visit(&mut |node| {
            let column = match node {
                ScalarExpr::InternalColumn(column) => Some(*column),
                ScalarExpr::Func {
                    binding: Some(binding),
                    ..
                } => uqa_sql::semantics::function_implicit_row_input(binding),
                _ => None,
            };
            if let Some(column) = column {
                required
                    .entry(column.relation())
                    .or_default()
                    .insert(column.attribute());
            }
        });
    });
    plan.visit_scoped_mut(&mut CapturedInputs {
        volatility,
        routines,
        required: &required,
    })
}

struct Rules<'a, 'b>(&'a RuleInputPlanningContext<'b>);
impl ScopedPlanVisitor for Rules<'_, '_> {
    fn command(
        &mut self,
        command: &mut CommandPlan,
        outer: &dyn Fn(&str) -> bool,
    ) -> Result<bool, SQLError> {
        if matches!(
            command,
            CommandPlan::Explain { .. } | CommandPlan::Prepare { .. }
        ) {
            return Ok(false);
        }
        prune_rule_inputs(self.0, command, outer)?;
        Ok(true)
    }
}

struct CapturedInputs<'a> {
    volatility: &'a dyn uqa_sql::semantics::volatility::VolatilityCatalog,
    routines: &'a dyn uqa_sql::routines::RoutineResolution,
    required: &'a std::collections::BTreeMap<uqa_sql::ast::InternalRelationId, BTreeSet<usize>>,
}
impl ScopedPlanVisitor for CapturedInputs<'_> {
    fn source(
        &mut self,
        source: &mut SourcePlan,
        visible: &dyn Fn(&str) -> bool,
    ) -> Result<bool, SQLError> {
        if let SourcePlan::Subquery {
            captured_output: Some(relation),
            body,
            ..
        } = source
        {
            if let RelationalPlan::QueryBlock(block) = &body.root {
                let width = block.projections.len();
                crate::mutation_outputs::prune_unused_query_outputs(
                    self.volatility,
                    self.routines,
                    body,
                    self.required.get(relation).unwrap_or(&BTreeSet::new()),
                    width,
                    visible,
                )?;
            }
        }
        Ok(true)
    }
}

fn prune_rule_inputs(
    context: &RuleInputPlanningContext<'_>,
    command: &mut CommandPlan,
    inherited_ctes: &dyn Fn(&str) -> bool,
) -> Result<(), SQLError> {
    let (table, bound, event) = match command {
        CommandPlan::Insert(plan) => (&plan.table, plan.target_relation_bound, RuleEvent::Insert),
        CommandPlan::Update(plan) => (&plan.table, plan.target_relation_bound, RuleEvent::Update),
        CommandPlan::Delete(plan) => (&plan.table, plan.target_relation_bound, RuleEvent::Delete),
        _ => return Ok(()),
    };
    let table = context.rules.resolve_mutation_target(table, bound)?;
    validate_rule_contracts(context, command, &table, event)?;
    if let CommandPlan::Update(plan) = command {
        uqa_sql::semantics::view_rewrite::validate_grouped_rule_inputs(
            context.views,
            &table,
            &plan.assignments,
        )?;
    }
    let Some(requirements) =
        uqa_sql::semantics::view_rewrite::rule_input_requirements(context.views, &table, event)?
    else {
        return Ok(());
    };
    uqa_sql::semantics::rules::validate_rule_returning_contract(
        context.rules,
        &table,
        event,
        command
            .returning()
            .is_some_and(|returning| !returning.is_empty()),
    )?;
    let requires_rows = requirements.requires_rows;
    let required = requirements.columns;
    match command {
        CommandPlan::Insert(plan) => {
            let columns = if plan.columns.is_empty() {
                context
                    .columns
                    .source_column_names(&table, true)?
                    .unwrap_or_default()
            } else {
                plan.columns
                    .iter()
                    .map(|target| target.column.clone())
                    .collect()
            };
            let positions = columns
                .iter()
                .enumerate()
                .filter_map(|(position, column)| required.contains(column).then_some(position))
                .collect::<BTreeSet<_>>();
            for (position, target) in plan.columns.iter_mut().enumerate() {
                if !positions.contains(&position) {
                    for expression in target.expressions_mut() {
                        *expression = ScalarExpr::Literal(Value::Null);
                    }
                }
            }
            if plan.rows.len() == 1 || !requires_rows {
                for row in &mut plan.rows {
                    for (position, expression) in row.iter_mut().enumerate() {
                        if !positions.contains(&position) {
                            *expression = ScalarExpr::Literal(Value::Null);
                        }
                    }
                }
            }
            if !requires_rows {
                plan.source = None;
                plan.ctes.clear();
                plan.subqueries.clear();
            } else if let Some(source) = &mut plan.source {
                crate::mutation_outputs::prune_unused_query_outputs(
                    context.volatility,
                    context.views.routines,
                    source,
                    &positions,
                    columns.len(),
                    &|name| plan.ctes.iter().any(|cte| cte.name == name) || inherited_ctes(name),
                )?;
            }
        }
        CommandPlan::Update(plan) => {
            prune_update_assignments(&mut plan.assignments, &required);
            if !requires_rows {
                plan.source = None;
                plan.predicate = None;
                plan.ctes.clear();
                plan.subqueries.clear();
            }
        }
        CommandPlan::Delete(plan) if !requires_rows => {
            plan.source = None;
            plan.predicate = None;
            plan.ctes.clear();
            plan.subqueries.clear();
        }
        _ => {}
    }
    Ok(())
}

fn prune_update_assignments(
    assignments: &mut Vec<uqa_sql::plan::AssignmentPlan>,
    required: &BTreeSet<String>,
) {
    *assignments = std::mem::take(assignments)
        .into_iter()
        .flat_map(|assignment| {
            if assignment
                .target
                .column_names()
                .all(|column| !required.contains(column))
            {
                assignment
                    .target
                    .targets()
                    .iter()
                    .cloned()
                    .map(|target| uqa_sql::plan::AssignmentPlan {
                        target: target.into(),
                        value: ScalarExpr::Literal(Value::Null),
                    })
                    .collect::<Vec<_>>()
            } else {
                vec![assignment]
            }
        })
        .collect();
    for assignment in assignments {
        if assignment
            .target
            .column_names()
            .all(|column| !required.contains(column))
        {
            for expression in assignment.expressions_mut() {
                *expression = ScalarExpr::Literal(Value::Null);
            }
        }
    }
}

fn validate_rule_contracts(
    context: &RuleInputPlanningContext<'_>,
    command: &CommandPlan,
    table: &str,
    event: RuleEvent,
) -> Result<(), SQLError> {
    if !command.ctes().is_empty() {
        uqa_sql::semantics::rules::validate_rule_cte_contract(
            context.rules,
            table,
            event,
            |table, event| {
                uqa_sql::semantics::view_rewrite::rule_inputs::original_rule_target(
                    context.views,
                    table,
                    event,
                )
            },
        )?;
    }
    if let CommandPlan::Insert(insert) = command {
        uqa_sql::semantics::rules::validate_insert_rule_query_contract(
            context.rules,
            table,
            insert,
            |table, event| {
                uqa_sql::semantics::view_rewrite::rule_inputs::original_rule_target(
                    context.views,
                    table,
                    event,
                )
            },
        )?;
    }
    Ok(())
}

#[cfg(test)]
mod tests;
