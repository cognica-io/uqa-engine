//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Rule visits retain the current original mutation before automatic view translation.

use super::super::{
    correlation::{self, CorrelatedDmlContext},
    layer_privileges::LayerPrivileges,
    returning::owned,
    validation::production as validation,
    view_not_updatable, AutomaticViewLayer, ColumnWrite, ExpressionScope, NotUpdatableReason,
    SQLError, ViewCommand, ViewRewriteBindings, ViewRewriteContext,
};
use super::{
    aliases, check_option, checks, finish_returning, layers, qualifier, returning, returning_count,
    table,
};
use crate::{
    ast::{RuleEvent, TriggerEvent, TriggerTiming},
    binding::production::BindingNameSet,
    plan::{CommandAssignmentSlot, CommandPlan, CommandScalarSlot, MutableCommandPlan},
};
use uqa_core::memory::{Produced, ProductionControl};

/// Each callback consumes the current relation's command. A suppressing rule can retire it before any lower view is examined.
#[expect(
    clippy::too_many_lines,
    reason = "preserves original qualification, view diagnostics and privilege order"
)]
pub fn rewrite_rule_inputs_owned(
    services: ViewRewriteContext<'_>,
    mut input: MutableCommandPlan,
    params: &[crate::SQLParam],
    inherited: Option<ViewRewriteBindings<'_>>,
    visit: &mut dyn FnMut(MutableCommandPlan) -> Result<Option<MutableCommandPlan>, SQLError>,
    control: &ProductionControl<'_>,
) -> Result<Option<MutableCommandPlan>, SQLError> {
    control.check()?;
    let (update, event, trigger, command) = match &*input {
        CommandPlan::Update(plan) => {
            validation::validate_public_view_targets(
                services,
                &plan.table,
                plan.assignments
                    .iter()
                    .flat_map(|assignment| assignment.target.column_names()),
                control,
            )?;
            (
                true,
                RuleEvent::Update,
                TriggerEvent::Update,
                ViewCommand::Update,
            )
        }
        CommandPlan::Delete(_) => (
            false,
            RuleEvent::Delete,
            TriggerEvent::Delete,
            ViewCommand::Delete,
        ),
        _ => {
            return Err(SQLError::Internal(
                "view rule rewrite requires UPDATE or DELETE".into(),
            ))
        }
    };
    let (source, ctes, subqueries) = match &*input {
        CommandPlan::Update(plan) => (
            plan.source.as_deref(),
            plan.ctes.as_slice(),
            plan.subqueries.as_slice(),
        ),
        CommandPlan::Delete(plan) => (
            plan.source.as_deref(),
            plan.ctes.as_slice(),
            plan.subqueries.as_slice(),
        ),
        _ => unreachable!(),
    };
    let source = correlation::schemas::source_schema(
        services, source, ctes, subqueries, params, inherited, control,
    )?;
    match &*input {
        CommandPlan::Update(plan) => {
            validation::validate_public_update_contract(services, plan, source.as_ref(), control)?;
        }
        CommandPlan::Delete(plan) => {
            validation::validate_public_delete_contract(services, plan, source.as_ref(), control)?;
        }
        _ => unreachable!(),
    }
    let mut privileges = LayerPrivileges::new();
    input = check_privileges(services, input, &mut privileges, control)?;
    let Some(retained) = visit(input)? else {
        privileges.finish()?;
        return Ok(None);
    };
    input = retained;
    let mut initial = if update {
        let view = services
            .catalog
            .try_resolve_view_name_with_control(table(&input), control)?
            .ok_or_else(|| SQLError::UnknownTable(table(&input).to_owned()))?;
        layers::direct_rule_path(services, &view, event, command, control)?;
        let layer = layers::rewritable(services, &view, command, control)?;
        writable_layer(&layer, command)?;
        let CommandPlan::Update(plan) = &*input else {
            unreachable!("UPDATE visit");
        };
        validation::view_targets(
            &layer,
            plan.assignments
                .iter()
                .flat_map(|assignment| assignment.target.targets()),
            super::super::duplicate_assignment,
            control,
        )?;
        Some(layer)
    } else {
        None
    };
    let mut cascaded = false;
    let mut visited = BindingNameSet::new();
    let mut depth = 0;
    let mut boundaries = control.finish(Vec::new(), control.empty_reservation())?;
    let mut suppressed = false;
    loop {
        control.check()?;
        if depth != 0
            && !suppressed
            && services.catalog.has_trigger_definition_with_control(
                table(&input),
                TriggerTiming::InsteadOf,
                trigger,
                true,
                control,
            )?
        {
            break;
        }
        if depth != 0 {
            let Some(retained) = visit(input)? else {
                privileges.finish()?;
                return Ok(None);
            };
            input = retained;
        }
        let Some(layer) = layers::next(
            services,
            table(&input),
            &mut initial,
            suppressed,
            event,
            command,
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
                .capture_rules_with_control(&layer.canonical_name, event, control)?
                .is_empty()?;
        if has_rules {
            input = input.record_view_rule_relation(&layer.canonical_name, control)?;
        }
        let layer_suppresses =
            has_rules && layers::suppresses(services, &layer.canonical_name, event, control)?;
        if depth > 1 && !suppressed && !layer_suppresses {
            if update {
                writable_layer(&layer, command)?;
            }
            input = check_privileges(services, input, &mut privileges, control)?;
        }
        if has_rules && layers::returns_rows(services, &layer.canonical_name, event, control)? {
            input = input.preserve_view_rule_returning(&layer.canonical_name, control)?;
        }
        if has_rules && update {
            input = input.record_view_update_inputs(&layer.canonical_name, control)?;
        }
        let target_qualifier = control.copy_text(qualifier(&input))?;
        if depth == 1 {
            match &*input {
                CommandPlan::Update(plan) => correlation::validation::validate_update_expressions(
                    services,
                    plan,
                    &layer,
                    source.as_ref(),
                    params,
                    inherited,
                    control,
                )?,
                CommandPlan::Delete(plan) => correlation::validation::validate_delete_expressions(
                    services,
                    plan,
                    &layer,
                    source.as_ref(),
                    params,
                    inherited,
                    control,
                )?,
                _ => unreachable!(),
            }
        }
        input = input.map_mutation_arena(control, |input, arena| {
            let (ids, ctes, returning) = match input {
                CommandPlan::Update(plan) => (
                    correlation::ids::update(plan, control)?,
                    plan.ctes.as_slice(),
                    plan.returning.as_slice(),
                ),
                CommandPlan::Delete(plan) => (
                    correlation::ids::delete(plan, control)?,
                    plan.ctes.as_slice(),
                    plan.returning.as_slice(),
                ),
                _ => unreachable!(),
            };
            correlation::rewrite(
                CorrelatedDmlContext {
                    services,
                    layer: &layer,
                    target_qualifier: &target_qualifier,
                    source: source.as_ref(),
                    returning_aliases: None,
                    include_excluded: false,
                    ctes,
                    inherited_ctes: inherited,
                    ids: &ids,
                    params,
                },
                arena,
                control,
            )?;
            let ids = correlation::ids::returning(returning, control)?;
            correlation::rewrite(
                CorrelatedDmlContext {
                    services,
                    layer: &layer,
                    target_qualifier: &target_qualifier,
                    source: source.as_ref(),
                    returning_aliases: Some(aliases(input)),
                    include_excluded: false,
                    ctes,
                    inherited_ctes: inherited,
                    ids: &ids,
                    params,
                },
                arena,
                control,
            )
        })?;
        let scope = ExpressionScope {
            target_qualifier: &target_qualifier,
            returning_aliases: None,
            source: source.as_ref(),
            include_excluded: false,
        };
        if let CommandPlan::Update(plan) = &*input {
            if !layer_suppresses && !suppressed {
                validation::writable_columns(
                    &layer,
                    plan.assignments
                        .iter()
                        .flat_map(|assignment| assignment.target.column_names()),
                    ColumnWrite::Update,
                    control,
                )?;
            }
            let count = plan.assignments.len();
            for index in 0..count {
                input = input.map_assignment_input(
                    CommandAssignmentSlot::Update(index),
                    control,
                    |input, arena| {
                        owned::target_expression(services, input, &layer, scope, arena, control)
                    },
                )?;
                if !layer_suppresses && !suppressed {
                    input = input.map_assignment_columns(
                        CommandAssignmentSlot::Update(index),
                        control,
                        |name| {
                            validation::writable_column(&layer, name, ColumnWrite::Update, control)
                        },
                    )?;
                }
            }
            let CommandPlan::Update(plan) = &*input else {
                unreachable!();
            };
            validation::validate_mapped_columns(
                plan.assignments
                    .iter()
                    .flat_map(|assignment| assignment.target.targets()),
                super::super::duplicate_assignment,
                control,
            )?;
        }
        input = input.map_predicate_input(control, |input, arena| {
            input
                .map(|input| {
                    owned::target_expression(services, input, &layer, scope, arena, control)
                })
                .transpose()
        })?;
        if update {
            input = checks(services, input, &layer, &target_qualifier, control)?;
        }
        let (retained, positions) =
            returning(services, input, &layer, source.as_ref(), false, control)?;
        input = retained;
        if depth == 1 {
            boundaries = positions;
        }
        input = input.map_predicate_input(control, |input, arena| {
            owned::combine_predicate(services, input, &layer, &target_qualifier, arena, control)
        })?;
        if update {
            input = check_option(
                services,
                input,
                &layer,
                &target_qualifier,
                &mut cascaded,
                control,
            )?;
        }
        input = input.retarget_mutation(
            control.copy_text(&layer.source_name)?,
            layer.source_include_descendants,
            control,
        )?;
        suppressed |= layer_suppresses;
        if services
            .catalog
            .target_view_kind_with_control(table(&input), control)?
            .is_none()
        {
            break;
        }
    }
    if update {
        input = input.finalize_view_update_inputs(control)?;
    }
    if let Some(source) = &source {
        let width = owned::target_width(services, table(&input), control)?;
        if let CommandPlan::Update(plan) = &*input {
            for index in 0..plan.assignments.len() {
                input = input.map_assignment(
                    CommandAssignmentSlot::Update(index),
                    control,
                    |input| owned::source_positions(input, source, width, control),
                )?;
            }
        }
        input = input.map_scalar(CommandScalarSlot::Predicate, control, |input| {
            owned::source_positions(input, source, width, control)
        })?;
        for index in 0..returning_count(&input) {
            input = input.map_scalar(CommandScalarSlot::Returning(index), control, |input| {
                owned::source_positions(input, source, width, control)
            })?;
        }
    }
    input = finish_returning(services, input, source.as_ref(), &boundaries, control)?;
    privileges.finish()?;
    Ok(Some(input))
}

fn writable_layer(layer: &AutomaticViewLayer, command: ViewCommand) -> Result<(), SQLError> {
    if !layer.has_writable_column() {
        return Err(view_not_updatable(
            &layer.canonical_name,
            command,
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
    let subject: Produced<_> = match &*input {
        CommandPlan::Update(plan) => {
            privileges.check_with_control(services.authorization, &plan.table, control, || {
                crate::semantics::view_privileges::ensure_update_with_control(
                    services.authorization,
                    plan,
                    control,
                )
            })?
        }
        CommandPlan::Delete(plan) => {
            privileges.check_with_control(services.authorization, &plan.table, control, || {
                crate::semantics::view_privileges::ensure_delete_with_control(
                    services.authorization,
                    plan,
                    control,
                )
            })?
        }
        _ => unreachable!("view UPDATE/DELETE"),
    };
    input.set_target_privilege_subject(subject, control)
}
