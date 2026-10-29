//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Rule pruning moves private constructors and keeps every dependency name and positional index admitted.

use super::RuleInputPlanningContext;
use uqa_core::{
    memory::{ProductionControl, ProductionMap},
    Value,
};
use uqa_sql::{
    ast::RuleEvent,
    plan::{
        CommandPlan, CommandQuerySlot, CommandScalarSlot, MutableCommandPlan,
        MutableExecutablePlan, MutableSourcePlan, OwnedScopedPlanVisitor, RelationalPlan,
        SourcePlan,
    },
    semantics::{rules, view_rewrite::rule_inputs::production},
    SQLError, ScalarExpr,
};

pub fn prune_captured_inputs_owned(
    volatility: &dyn uqa_sql::semantics::volatility::VolatilityCatalog,
    routines: &dyn uqa_sql::routines::RoutineResolution,
    plan: MutableExecutablePlan,
    control: &ProductionControl<'_>,
) -> Result<MutableExecutablePlan, SQLError> {
    let mut required = ProductionMap::new(*control);
    let mut result = Ok(());
    plan.as_plan_ref()
        .visit_scalar_expressions(&mut |expression| {
            if result.is_err() {
                return;
            }
            result = expression.try_visit(&mut |node| {
                control.check()?;
                let column = match node {
                    ScalarExpr::InternalColumn(column) => Some(*column),
                    ScalarExpr::Func {
                        binding: Some(binding),
                        ..
                    } => uqa_sql::semantics::function_implicit_row_input(binding),
                    _ => None,
                };
                if let Some(column) = column {
                    required.insert_copy(column, ())?;
                }
                Ok::<_, SQLError>(true)
            });
        });
    result?;
    plan.visit_scoped(
        control,
        &mut CapturedInputs {
            volatility,
            routines,
            required,
            control,
        },
    )
}

struct CapturedInputs<'a, 'c> {
    volatility: &'a dyn uqa_sql::semantics::volatility::VolatilityCatalog,
    routines: &'a dyn uqa_sql::routines::RoutineResolution,
    required: ProductionMap<'c, uqa_sql::ast::InternalColumnRef, ()>,
    control: &'a ProductionControl<'c>,
}
impl OwnedScopedPlanVisitor for CapturedInputs<'_, '_> {
    fn source(
        &mut self,
        source: MutableSourcePlan,
        visible: &dyn Fn(&str) -> bool,
    ) -> Result<(MutableSourcePlan, bool), SQLError> {
        let selected = match &*source {
            SourcePlan::Subquery {
                captured_output: Some(relation),
                body,
                ..
            } => match &body.root {
                RelationalPlan::QueryBlock(block) => Some((*relation, block.projections.len())),
                _ => None,
            },
            _ => None,
        };
        let source = match selected {
            Some((relation, width)) => source.map_query(self.control, |query| {
                crate::mutation_outputs::prune_unused_query_outputs_owned(
                    self.volatility,
                    self.routines,
                    query,
                    &|position| self.required.get(&relation.column(position)).is_some(),
                    width,
                    visible,
                    self.control,
                )
            })?,
            None => source,
        };
        Ok((source, true))
    }
}

pub fn prune_command_inputs_owned(
    context: &RuleInputPlanningContext<'_>,
    plan: MutableExecutablePlan,
    control: &ProductionControl<'_>,
) -> Result<MutableExecutablePlan, SQLError> {
    plan.visit_scoped(control, &mut Rules { context, control })
}

pub(super) struct Rules<'a, 'b, 'c> {
    pub context: &'a RuleInputPlanningContext<'b>,
    pub control: &'a ProductionControl<'c>,
}
impl OwnedScopedPlanVisitor for Rules<'_, '_, '_> {
    fn command(
        &mut self,
        command: MutableCommandPlan,
        outer: &dyn Fn(&str) -> bool,
    ) -> Result<(MutableCommandPlan, bool), SQLError> {
        if matches!(
            &*command,
            CommandPlan::Explain { .. } | CommandPlan::Prepare { .. }
        ) {
            return Ok((command, false));
        }
        Ok((
            prune_rule_inputs(self.context, command, outer, self.control)?,
            true,
        ))
    }
}

fn prune_rule_inputs(
    context: &RuleInputPlanningContext<'_>,
    mut command: MutableCommandPlan,
    inherited_ctes: &dyn Fn(&str) -> bool,
    control: &ProductionControl<'_>,
) -> Result<MutableCommandPlan, SQLError> {
    let (table, bound, event) = match &*command {
        CommandPlan::Insert(plan) => (&plan.table, plan.target_relation_bound, RuleEvent::Insert),
        CommandPlan::Update(plan) => (&plan.table, plan.target_relation_bound, RuleEvent::Update),
        CommandPlan::Delete(plan) => (&plan.table, plan.target_relation_bound, RuleEvent::Delete),
        _ => return Ok(command),
    };
    let table = context
        .rules
        .resolve_mutation_target_with_control(table, bound, control)?;
    validate_rule_contracts(context, &command, &table, event, control)?;
    if let CommandPlan::Update(plan) = &*command {
        production::validate_grouped_rule_inputs_with_control(
            context.views,
            &table,
            &plan.assignments,
            control,
        )?;
    }
    let Some(required) =
        production::rule_input_requirements_with_control(context.views, &table, event, control)?
    else {
        return Ok(command);
    };
    rules::validate_rule_returning_contract_with_control(
        context.rules,
        &table,
        event,
        command.returning().is_some_and(|output| !output.is_empty()),
        control,
    )?;
    match &*command {
        CommandPlan::Insert(_) => {
            command = prune_insert(context, command, &table, &required, inherited_ctes, control)?;
        }
        CommandPlan::Update(_) => {
            command = command
                .prune_rule_assignments(&|name| required.columns.contains_key(name), control)?;
            if !required.requires_rows {
                command = command.discard_rule_rows(control)?;
            }
        }
        CommandPlan::Delete(_) if !required.requires_rows => {
            command = command.discard_rule_rows(control)?;
        }
        _ => {}
    }
    Ok(command)
}

fn prune_insert(
    context: &RuleInputPlanningContext<'_>,
    mut command: MutableCommandPlan,
    table: &str,
    required: &uqa_sql::semantics::view_rewrite::rule_inputs::production::OwnedRuleInputRequirements,
    inherited_ctes: &dyn Fn(&str) -> bool,
    control: &ProductionControl<'_>,
) -> Result<MutableCommandPlan, SQLError> {
    let CommandPlan::Insert(plan) = &*command else {
        unreachable!()
    };
    let mut positions = ProductionMap::new(*control);
    let width = if plan.columns.is_empty() {
        let columns = context
            .columns
            .source_column_names_with_control(table, true, control)?;
        if let Some(columns) = &columns {
            for (position, column) in columns.iter().enumerate() {
                control.check()?;
                if required.columns.contains_key(column.as_str()) {
                    positions.insert_copy(position, ())?;
                }
            }
        }
        columns.as_ref().map_or(0, |columns| columns.len())
    } else {
        for (position, target) in plan.columns.iter().enumerate() {
            control.check()?;
            if required.columns.contains_key(target.column.as_str()) {
                positions.insert_copy(position, ())?;
            }
        }
        plan.columns.len()
    };
    let targets = plan.columns.len();
    let rows = plan.rows.len();
    for target in 0..targets {
        if positions.get(&target).is_none() {
            command = command.map_insert_target(target, control, null)?;
        }
    }
    if rows == 1 || !required.requires_rows {
        for row in 0..rows {
            let CommandPlan::Insert(plan) = &*command else {
                unreachable!()
            };
            for column in 0..plan.rows[row].len() {
                if positions.get(&column).is_none() {
                    command = command.map_scalar(
                        CommandScalarSlot::InsertValue { row, column },
                        control,
                        |old| null(old, control),
                    )?;
                }
            }
        }
    }
    if !required.requires_rows {
        return command.discard_rule_rows(control);
    }
    let CommandPlan::Insert(plan) = &*command else {
        unreachable!()
    };
    if plan.source.is_some() {
        let mut names = uqa_core::memory::ProductionVec::new(*control);
        for cte in &plan.ctes {
            names.push_produced(control.copy_text(&cte.name)?)?;
        }
        let names = names.finish()?;
        command = command.map_query_child(CommandQuerySlot::InsertSource, control, |source| {
            crate::mutation_outputs::prune_unused_query_outputs_owned(
                context.volatility,
                context.views.routines,
                source,
                &|position| positions.get(&position).is_some(),
                width,
                &|name| names.iter().any(|candidate| candidate == name) || inherited_ctes(name),
                control,
            )
        })?;
    }
    Ok(command)
}

fn null(
    old: uqa_sql::plan::OwnedScalarExpr,
    control: &ProductionControl<'_>,
) -> Result<uqa_sql::plan::OwnedScalarExpr, SQLError> {
    drop(old);
    Ok(ScalarExpr::Literal(Value::Null).copy_for_rewrite(control)?)
}

fn validate_rule_contracts(
    context: &RuleInputPlanningContext<'_>,
    command: &CommandPlan,
    table: &str,
    event: RuleEvent,
    control: &ProductionControl<'_>,
) -> Result<(), SQLError> {
    if !command.ctes().is_empty() {
        rules::validate_rule_cte_contract_with_control(
            context.rules,
            table,
            event,
            |table, event| {
                production::original_rule_target_with_control(context.views, table, event, control)
            },
            control,
        )?;
    }
    if let CommandPlan::Insert(insert) = command {
        rules::validate_insert_rule_query_contract_with_control(
            context.rules,
            table,
            insert,
            |table, event| {
                production::original_rule_target_with_control(context.views, table, event, control)
            },
            control,
        )?;
    }
    Ok(())
}
