//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! MERGE keeps its action and scalar-query owners through automatic view translation.

use super::super::{
    correlation::{self, CorrelatedDmlContext},
    layer_privileges::LayerPrivileges,
    returning::owned,
    validation::production as validation,
    AutomaticViewLayer, ColumnWrite, ExpressionScope, MergeViewTargetPath, SQLError,
    ViewRewriteBindings, ViewRewriteContext,
};
use super::{check_option, checks, finish_returning, merge_validation, returning};
use crate::{
    binding::production::BindingNameSet,
    plan::{
        CommandAssignmentSlot, CommandPlan, CommandScalarSlot, MergePlan, MergeWhenPlan,
        MutableCommandPlan,
    },
};
use uqa_core::memory::{Produced, ProductionControl};

fn merge(input: &CommandPlan) -> &MergePlan {
    let CommandPlan::Merge(plan) = input else {
        unreachable!("MERGE view command")
    };
    plan
}

fn layer(
    services: ViewRewriteContext<'_>,
    name: &str,
    control: &ProductionControl<'_>,
) -> Result<Produced<AutomaticViewLayer>, SQLError> {
    match super::super::production::view_query_shape_owned(services, name, control)? {
        Some(super::super::production::OwnedViewQueryShape::Updatable(layer)) => {
            layer.finish(control)
        }
        _ => Err(SQLError::Internal(format!(
            "automatic MERGE rewrite selected for view `{name}`, which cannot be rewritten"
        ))),
    }
}

#[expect(
    clippy::too_many_lines,
    reason = "preserves MERGE source, clause, row image and layer privilege order"
)]
pub fn rewrite_merge_owned(
    services: ViewRewriteContext<'_>,
    mut input: MutableCommandPlan,
    params: &[crate::SQLParam],
    inherited: Option<ViewRewriteBindings<'_>>,
    control: &ProductionControl<'_>,
) -> Result<MutableCommandPlan, SQLError> {
    control.check()?;
    let plan = merge(&input);
    if services
        .catalog
        .target_view_kind_with_control(&plan.target, control)?
        == Some(crate::catalog::view::StoredViewKind::Materialized)
    {
        return Err(super::super::updatability::merge_into_materialized_view(
            &plan.target,
        ));
    }
    let source = correlation::schemas::source_schema(
        services,
        Some(&plan.source),
        &plan.ctes,
        &plan.subqueries,
        params,
        inherited,
        control,
    )?
    .expect("MERGE source");
    merge_validation::validate_public_merge_targets_with_control(services, plan, control)?;
    validation::validate_public_merge_contract(services, plan, &source, control)?;
    if merge_validation::merge_view_target_path_with_control(services, plan, control)?
        != MergeViewTargetPath::AutomaticRewrite
    {
        return Err(SQLError::Internal(
            "automatic MERGE rewrite selected for a view-trigger target".into(),
        ));
    }
    let initial = layer(services, &plan.target, control)?;
    merge_validation::targets(&initial, plan, control)?;
    correlation::validation::validate_merge_expressions(
        services, plan, &initial, &source, params, inherited, control,
    )?;
    let mut initial = Some(initial);
    let mut privileges = LayerPrivileges::new();
    input = check_privileges(services, input, &mut privileges, control)?;
    let mut cascaded = false;
    let mut visited = BindingNameSet::new();
    let mut depth = 0;
    let mut boundaries = control.finish(Vec::new(), control.empty_reservation())?;
    loop {
        control.check()?;
        if depth != 0
            && merge_validation::merge_view_target_path_with_control(
                services,
                merge(&input),
                control,
            )? == MergeViewTargetPath::ViewTriggers
        {
            break;
        }
        let layer = match initial.take() {
            Some(layer) => layer,
            None => layer(services, &merge(&input).target, control)?,
        };
        if !visited.insert_with_control(&layer.canonical_name, control)? {
            return Err(SQLError::Internal(format!(
                "cycle while rewriting automatically updatable view `{}`",
                layer.canonical_name
            )));
        }
        depth += 1;
        merge_validation::targets(&layer, merge(&input), control)?;
        merge_validation::writable(&layer, merge(&input), control)?;
        if depth > 1 {
            input = check_privileges(services, input, &mut privileges, control)?;
        }
        let qualifier = control.copy_text(&merge(&input).target_qualifier)?;
        input = input.map_mutation_arena(control, |command, arena| {
            let plan = merge(command);
            let ids = correlation::ids::merge_matched(plan, control)?;
            let context = CorrelatedDmlContext {
                services,
                layer: &layer,
                target_qualifier: &qualifier,
                source: Some(&source),
                returning_aliases: None,
                include_excluded: false,
                ctes: &plan.ctes,
                inherited_ctes: inherited,
                ids: &ids,
                params,
            };
            correlation::rewrite(context, arena, control)?;
            let ids = correlation::ids::merge_target_only(plan, control)?;
            correlation::rewrite(
                CorrelatedDmlContext {
                    source: None,
                    ids: &ids,
                    ..context
                },
                arena,
                control,
            )?;
            let ids = correlation::ids::returning(&plan.returning, control)?;
            correlation::rewrite(
                CorrelatedDmlContext {
                    returning_aliases: Some(&plan.returning_aliases),
                    ids: &ids,
                    ..context
                },
                arena,
                control,
            )
        })?;
        let matched = ExpressionScope {
            target_qualifier: &qualifier,
            returning_aliases: None,
            source: Some(&source),
            include_excluded: false,
        };
        let target_only = ExpressionScope {
            source: None,
            ..matched
        };
        input = input.map_scalar_input(CommandScalarSlot::MergeJoin, control, |input, arena| {
            owned::target_expression(services, input, &layer, matched, arena, control)
        })?;
        input = input.map_predicate_input(control, |input, arena| {
            input
                .map(|input| {
                    owned::target_expression(services, input, &layer, target_only, arena, control)
                })
                .transpose()
        })?;
        for index in 0..merge(&input).when_clauses.len() {
            input = clause(
                services,
                input,
                index,
                &layer,
                matched,
                target_only,
                control,
            )?;
        }
        input = checks(services, input, &layer, &qualifier, control)?;
        let (retained, positions) =
            returning(services, input, &layer, Some(&source), true, control)?;
        input = retained;
        if depth == 1 {
            boundaries = positions;
        }
        input = input.map_predicate_input(control, |input, arena| {
            owned::combine_predicate(services, input, &layer, &qualifier, arena, control)
        })?;
        input = check_option(services, input, &layer, &qualifier, &mut cascaded, control)?;
        input = input.retarget_mutation(
            control.copy_text(&layer.source_name)?,
            layer.source_include_descendants,
            control,
        )?;
        if services
            .catalog
            .target_view_kind_with_control(&merge(&input).target, control)?
            .is_none()
        {
            break;
        }
    }
    input = finish_returning(services, input, Some(&source), &boundaries, control)?;
    privileges.finish()?;
    Ok(input)
}

fn clause(
    services: ViewRewriteContext<'_>,
    mut input: MutableCommandPlan,
    index: usize,
    layer: &AutomaticViewLayer,
    matched: ExpressionScope<'_>,
    target_only: ExpressionScope<'_>,
    control: &ProductionControl<'_>,
) -> Result<MutableCommandPlan, SQLError> {
    let (scope, assignments) = match &merge(&input).when_clauses[index] {
        MergeWhenPlan::UpdateMatched { assignments, .. } => (matched, assignments.len()),
        MergeWhenPlan::UpdateNotMatchedBySource { assignments, .. } => {
            (target_only, assignments.len())
        }
        MergeWhenPlan::DeleteMatched { .. } | MergeWhenPlan::NothingMatched { .. } => (matched, 0),
        MergeWhenPlan::DeleteNotMatchedBySource { .. }
        | MergeWhenPlan::NothingNotMatchedBySource { .. } => (target_only, 0),
        MergeWhenPlan::InsertNotMatched {
            columns, values, ..
        } => {
            let implicit = columns.is_empty();
            let count = values.len();
            if implicit {
                input = input.set_implicit_merge_columns(
                    index,
                    layer
                        .columns
                        .iter()
                        .take(count)
                        .map(|column| column.name.as_str()),
                    control,
                )?;
            }
            let MergeWhenPlan::InsertNotMatched { columns, .. } =
                &merge(&input).when_clauses[index]
            else {
                unreachable!()
            };
            for target in 0..columns.len() {
                input = input.map_merge_insert_column(index, target, control, |name| {
                    validation::writable_column(layer, name, ColumnWrite::Merge, control)
                })?;
            }
            let MergeWhenPlan::InsertNotMatched { columns, .. } =
                &merge(&input).when_clauses[index]
            else {
                unreachable!()
            };
            validation::validate_mapped_columns(
                columns,
                super::super::duplicate_insert_column,
                control,
            )?;
            return Ok(input);
        }
        MergeWhenPlan::NothingNotMatched { .. } => return Ok(input),
    };
    input = input.map_scalar_input(
        CommandScalarSlot::MergeCondition(index),
        control,
        |input, arena| owned::target_expression(services, input, layer, scope, arena, control),
    )?;
    for assignment in 0..assignments {
        let slot = CommandAssignmentSlot::Merge {
            clause: index,
            assignment,
        };
        input = input.map_assignment_input(slot, control, |input, arena| {
            owned::target_expression(services, input, layer, scope, arena, control)
        })?;
        input = input.map_assignment_columns(slot, control, |name| {
            validation::writable_column(layer, name, ColumnWrite::Merge, control)
        })?;
    }
    if let MergeWhenPlan::UpdateMatched { assignments, .. }
    | MergeWhenPlan::UpdateNotMatchedBySource { assignments, .. } =
        &merge(&input).when_clauses[index]
    {
        validation::validate_mapped_columns(
            assignments
                .iter()
                .flat_map(|assignment| assignment.target.targets()),
            super::super::duplicate_assignment,
            control,
        )?;
    }
    Ok(input)
}

fn check_privileges(
    services: ViewRewriteContext<'_>,
    input: MutableCommandPlan,
    privileges: &mut LayerPrivileges,
    control: &ProductionControl<'_>,
) -> Result<MutableCommandPlan, SQLError> {
    let plan = merge(&input);
    let subject =
        privileges.check_with_control(services.authorization, &plan.target, control, || {
            crate::semantics::view_privileges::ensure_merge_with_control(
                services.authorization,
                plan,
                control,
            )
        })?;
    input.set_target_privilege_subject(subject, control)
}
