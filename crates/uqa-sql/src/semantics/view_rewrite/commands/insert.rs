//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! INSERT retains its command and each view-rule namespace until the base target executes.

use super::super::{
    correlation::{self, CorrelatedDmlContext},
    layer_privileges::LayerPrivileges,
    returning::owned,
    validation::production as validation,
    view_not_updatable, ColumnWrite, ExpressionScope, NotUpdatableReason, SQLError, ViewCommand,
    ViewRewriteBindings, ViewRewriteContext,
};
use super::{check_option, checks, layers, returning};
use crate::{
    ast::{RuleEvent, TriggerEvent, TriggerTiming},
    binding::production::BindingNameSet,
    plan::{
        CommandAssignmentSlot, CommandPlan, CommandScalarSlot, ConflictActionPlan, InsertPlan,
        MutableCommandPlan,
    },
};
use uqa_core::memory::ProductionControl;

fn insert(input: &CommandPlan) -> &InsertPlan {
    let CommandPlan::Insert(plan) = input else {
        unreachable!("INSERT view command")
    };
    plan
}

#[expect(
    clippy::too_many_lines,
    reason = "preserves INSERT target, rule, expression and layer privilege order"
)]
pub fn rewrite_insert_owned(
    services: ViewRewriteContext<'_>,
    mut input: MutableCommandPlan,
    params: &[crate::SQLParam],
    inherited: Option<ViewRewriteBindings<'_>>,
    control: &ProductionControl<'_>,
) -> Result<MutableCommandPlan, SQLError> {
    control.check()?;
    let plan = insert(&input);
    validation::validate_public_view_targets(
        services,
        &plan.table,
        plan.columns.iter().map(|target| target.column.as_str()),
        control,
    )?;
    validation::validate_public_insert_contract(services, plan, control)?;
    let view = services
        .catalog
        .try_resolve_view_name_with_control(&plan.table, control)?
        .ok_or_else(|| SQLError::UnknownTable(plan.table.clone()))?;
    layers::direct_rule_path(
        services,
        &view,
        RuleEvent::Insert,
        ViewCommand::Insert,
        control,
    )?;
    let initial = layers::rewritable(services, &view, ViewCommand::Insert, control)?;
    writable_layer(&initial)?;
    validation::insert_targets(&initial, plan, control)?;
    let mut initial = Some(initial);
    let mut privileges = LayerPrivileges::new();
    input = check_privileges(services, input, &mut privileges, control)?;
    let mut implicit_width = if insert(&input).columns.is_empty() {
        Some(correlation::schemas::insert_input_width(
            services,
            insert(&input),
            params,
            inherited,
            control,
        )?)
    } else {
        None
    };
    let mut cascaded = false;
    let mut visited = BindingNameSet::new();
    let mut depth = 0;
    let mut suppressed = false;
    loop {
        control.check()?;
        if depth != 0
            && !suppressed
            && services.catalog.has_trigger_definition_with_control(
                &insert(&input).table,
                TriggerTiming::InsteadOf,
                TriggerEvent::Insert,
                true,
                control,
            )?
        {
            break;
        }
        let Some(layer) = layers::next(
            services,
            &insert(&input).table,
            &mut initial,
            suppressed,
            RuleEvent::Insert,
            ViewCommand::Insert,
            control,
        )?
        else {
            break;
        };
        if !visited.insert_with_control(&layer.canonical_name, control)? {
            return Err(SQLError::Internal(format!(
                "cycle while rewriting automatically updatable view `{}`",
                layer.canonical_name
            )));
        }
        depth += 1;
        let has_rules = !suppressed
            && !services
                .catalog
                .capture_rules_with_control(&layer.canonical_name, RuleEvent::Insert, control)?
                .is_empty()?;
        if has_rules {
            input = input.record_view_rule_relation(&layer.canonical_name, control)?;
        }
        let layer_suppresses = has_rules
            && layers::suppresses(services, &layer.canonical_name, RuleEvent::Insert, control)?;
        if depth > 1 && !suppressed && !layer_suppresses {
            writable_layer(&layer)?;
            input = check_privileges(services, input, &mut privileges, control)?;
        }
        if has_rules
            && layers::returns_rows(services, &layer.canonical_name, RuleEvent::Insert, control)?
        {
            input = input.preserve_view_rule_returning(&layer.canonical_name, control)?;
        }
        let qualifier = control.copy_text(&insert(&input).target_qualifier)?;
        if depth == 1 {
            correlation::validation::validate_insert_expressions(
                services,
                insert(&input),
                &layer,
                params,
                inherited,
                control,
            )?;
        }
        input = input.map_mutation_arena(control, |command, arena| {
            let plan = insert(command);
            let ids = correlation::ids::conflict(plan, control)?;
            let context = CorrelatedDmlContext {
                services,
                layer: &layer,
                target_qualifier: &qualifier,
                source: None,
                returning_aliases: None,
                include_excluded: true,
                ctes: &plan.ctes,
                inherited_ctes: inherited,
                ids: &ids,
                params,
            };
            correlation::rewrite(context, arena, control)?;
            let ids = correlation::ids::returning(&plan.returning, control)?;
            correlation::rewrite(
                CorrelatedDmlContext {
                    returning_aliases: Some(&plan.returning_aliases),
                    include_excluded: false,
                    ids: &ids,
                    ..context
                },
                arena,
                control,
            )
        })?;
        if let Some(width) = implicit_width.take() {
            input = input.set_implicit_insert_columns(
                layer
                    .columns
                    .iter()
                    .take(width)
                    .map(|column| column.name.as_str()),
                control,
            )?;
        }
        if !suppressed && !layer_suppresses {
            let plan = insert(&input);
            let updates = match plan.on_conflict.as_ref().map(|conflict| &conflict.action) {
                Some(ConflictActionPlan::Update { assignments, .. }) => assignments.as_slice(),
                _ => &[],
            };
            validation::writable_columns(
                &layer,
                plan.columns
                    .iter()
                    .map(|target| target.column.as_str())
                    .chain(
                        updates
                            .iter()
                            .flat_map(|assignment| assignment.target.column_names()),
                    ),
                ColumnWrite::Insert,
                control,
            )?;
        }
        if has_rules {
            input = input.record_view_insert_inputs(&layer.canonical_name, control)?;
        }
        if !suppressed && !layer_suppresses {
            for index in 0..insert(&input).columns.len() {
                input = input.map_insert_column(index, control, |name| {
                    validation::writable_column(&layer, name, ColumnWrite::Insert, control)
                })?;
            }
        }
        validation::validate_mapped_columns(
            &insert(&input).columns,
            super::super::duplicate_assignment,
            control,
        )?;
        if let Some(conflict) = &insert(&input).on_conflict {
            let count = conflict.expressions.len();
            let scope = ExpressionScope {
                target_qualifier: &qualifier,
                returning_aliases: None,
                source: None,
                include_excluded: false,
            };
            for index in 0..count {
                input = input.map_scalar_input(
                    CommandScalarSlot::ConflictExpression(index),
                    control,
                    |input, arena| {
                        owned::target_expression(services, input, &layer, scope, arena, control)
                    },
                )?;
            }
            input = input.map_scalar_input(
                CommandScalarSlot::ConflictInferencePredicate,
                control,
                |input, arena| {
                    owned::target_expression(services, input, &layer, scope, arena, control)
                },
            )?;
            input = input.map_conflict_columns(control, |name| {
                validation::writable_column(&layer, name, ColumnWrite::Insert, control)
            })?;
            if let ConflictActionPlan::Update { assignments, .. } = &insert(&input)
                .on_conflict
                .as_ref()
                .expect("conflict")
                .action
            {
                let count = assignments.len();
                let scope = ExpressionScope {
                    include_excluded: true,
                    ..scope
                };
                for index in 0..count {
                    input = input.map_assignment_columns(
                        CommandAssignmentSlot::Conflict(index),
                        control,
                        |name| {
                            validation::writable_column(&layer, name, ColumnWrite::Insert, control)
                        },
                    )?;
                    input = input.map_assignment_input(
                        CommandAssignmentSlot::Conflict(index),
                        control,
                        |input, arena| {
                            owned::target_expression(services, input, &layer, scope, arena, control)
                        },
                    )?;
                }
                let ConflictActionPlan::Update { assignments, .. } = &insert(&input)
                    .on_conflict
                    .as_ref()
                    .expect("conflict")
                    .action
                else {
                    unreachable!()
                };
                validation::validate_mapped_columns(
                    assignments
                        .iter()
                        .flat_map(|assignment| assignment.target.targets()),
                    super::super::duplicate_assignment,
                    control,
                )?;
                input = input.map_scalar_input(
                    CommandScalarSlot::ConflictPredicate,
                    control,
                    |input, arena| {
                        owned::target_expression(services, input, &layer, scope, arena, control)
                    },
                )?;
            }
        }
        input = checks(services, input, &layer, &qualifier, control)?;
        input = returning(services, input, &layer, None, false, control)?.0;
        input = check_option(services, input, &layer, &qualifier, &mut cascaded, control)?;
        input = input.retarget_mutation(control.copy_text(&layer.source_name)?, true, control)?;
        suppressed |= layer_suppresses;
        if services
            .catalog
            .target_view_kind_with_control(&insert(&input).table, control)?
            .is_none()
        {
            break;
        }
    }
    input = input.finalize_view_insert_inputs(control)?;
    privileges.finish()?;
    Ok(input)
}

fn writable_layer(layer: &super::super::AutomaticViewLayer) -> Result<(), SQLError> {
    if !layer.has_writable_column() {
        return Err(view_not_updatable(
            &layer.canonical_name,
            ViewCommand::Insert,
            NotUpdatableReason::NoUpdatableColumns,
        ));
    }
    Ok(())
}

fn check_privileges(
    services: ViewRewriteContext<'_>,
    input: MutableCommandPlan,
    privileges: &mut LayerPrivileges,
    control: &ProductionControl<'_>,
) -> Result<MutableCommandPlan, SQLError> {
    let plan = insert(&input);
    let subject =
        privileges.check_with_control(services.authorization, &plan.table, control, || {
            crate::semantics::view_privileges::ensure_insert_with_control(
                services.authorization,
                plan,
                control,
            )
        })?;
    input.set_target_privilege_subject(subject, control)
}
