//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! MERGE selects its existing trigger or automatic path using borrowed view definitions and admitted scratch.

use super::super::{
    production::{view_query_shape_owned, OwnedViewQueryShape},
    updatability::{merge_into_materialized_view, merge_with_rules, mixed_merge_paths},
    validation::{merge_clause_command, production as validation},
    view_not_updatable, AutomaticViewLayer, ColumnWrite, MergeViewTargetPath, NotUpdatableReason,
    SQLError, ViewRewriteContext,
};
use crate::{
    ast::{RuleEvent, TriggerTiming},
    plan::{MergePlan, MergeWhenPlan},
};
use uqa_core::memory::{ProductionControl, ProductionVec};

pub fn merge_view_target_path_with_control(
    services: ViewRewriteContext<'_>,
    plan: &MergePlan,
    control: &ProductionControl<'_>,
) -> Result<MergeViewTargetPath, SQLError> {
    let canonical = services
        .catalog
        .try_resolve_view_name_with_control(&plan.target, control)?
        .ok_or_else(|| SQLError::UnknownTable(plan.target.clone()))?;
    services
        .catalog
        .with_view_definition_with_control(&canonical, control, &mut |definition| {
            let definition =
                definition.ok_or_else(|| SQLError::UnknownTable(plan.target.clone()))?;
            if definition.kind == crate::catalog::view::StoredViewKind::Materialized {
                return Err(merge_into_materialized_view(&canonical));
            }
            Ok(())
        })?;
    let mut has_rules = false;
    for event in [RuleEvent::Insert, RuleEvent::Update, RuleEvent::Delete] {
        has_rules |= !services
            .catalog
            .capture_rules_with_control(&canonical, event, control)?
            .is_empty()?;
    }
    if has_rules {
        return Err(merge_with_rules(&canonical));
    }
    let mut untriggered = None;
    let mut triggered = false;
    for clause in &plan.when_clauses {
        control.check()?;
        let Some((event, command)) = merge_clause_command(clause) else {
            continue;
        };
        if services.catalog.has_trigger_definition_with_control(
            &canonical,
            TriggerTiming::InsteadOf,
            event,
            true,
            control,
        )? {
            triggered = true;
        } else if untriggered.is_none() {
            untriggered = Some(command);
        }
    }
    let Some(untriggered) = untriggered else {
        return Ok(MergeViewTargetPath::ViewTriggers);
    };
    let writes_columns = plan.when_clauses.iter().any(|clause| {
        matches!(
            clause,
            MergeWhenPlan::InsertNotMatched { .. }
                | MergeWhenPlan::UpdateMatched { .. }
                | MergeWhenPlan::UpdateNotMatchedBySource { .. }
        )
    });
    let reason = match view_query_shape_owned(services, &canonical, control)? {
        Some(OwnedViewQueryShape::Updatable(layer)) => (writes_columns
            && !layer
                .columns
                .iter()
                .any(|column| column.writable_source_column.is_some()))
        .then_some(NotUpdatableReason::NoUpdatableColumns),
        Some(OwnedViewQueryShape::NotUpdatable(reason)) => Some(reason),
        None => return Err(SQLError::UnknownTable(plan.target.clone())),
    };
    if let Some(reason) = reason {
        return Err(view_not_updatable(&canonical, untriggered, reason));
    }
    if triggered {
        return Err(mixed_merge_paths(&canonical));
    }
    Ok(MergeViewTargetPath::AutomaticRewrite)
}

pub fn validate_public_merge_targets_with_control(
    services: ViewRewriteContext<'_>,
    plan: &MergePlan,
    control: &ProductionControl<'_>,
) -> Result<(), SQLError> {
    for clause in &plan.when_clauses {
        control.check()?;
        match clause {
            MergeWhenPlan::UpdateMatched { assignments, .. }
            | MergeWhenPlan::UpdateNotMatchedBySource { assignments, .. } => {
                validation::validate_public_view_targets(
                    services,
                    &plan.target,
                    assignments
                        .iter()
                        .flat_map(|assignment| assignment.target.column_names()),
                    control,
                )?;
                validation::validate_mapped_columns(
                    assignments
                        .iter()
                        .flat_map(|assignment| assignment.target.targets()),
                    super::super::duplicate_assignment,
                    control,
                )?;
            }
            MergeWhenPlan::InsertNotMatched { columns, .. } if !columns.is_empty() => {
                validation::validate_public_view_targets(
                    services,
                    &plan.target,
                    columns.iter().map(|target| target.column.as_str()),
                    control,
                )?;
                validation::validate_mapped_columns(
                    columns,
                    super::super::duplicate_insert_column,
                    control,
                )?;
            }
            _ => {}
        }
    }
    Ok(())
}

pub(super) fn targets(
    layer: &AutomaticViewLayer,
    plan: &MergePlan,
    control: &ProductionControl<'_>,
) -> Result<(), SQLError> {
    for clause in &plan.when_clauses {
        control.check()?;
        match clause {
            MergeWhenPlan::UpdateMatched { assignments, .. }
            | MergeWhenPlan::UpdateNotMatchedBySource { assignments, .. } => {
                validation::view_targets(
                    layer,
                    assignments
                        .iter()
                        .flat_map(|assignment| assignment.target.targets()),
                    super::super::duplicate_assignment,
                    control,
                )?;
            }
            MergeWhenPlan::InsertNotMatched { columns, .. } if !columns.is_empty() => {
                validation::view_targets(
                    layer,
                    columns.iter(),
                    super::super::duplicate_insert_column,
                    control,
                )?;
            }
            _ => {}
        }
    }
    Ok(())
}

pub(super) fn writable(
    layer: &AutomaticViewLayer,
    plan: &MergePlan,
    control: &ProductionControl<'_>,
) -> Result<(), SQLError> {
    let mut modified = ProductionVec::new(*control);
    for clause in &plan.when_clauses {
        control.check()?;
        match clause {
            MergeWhenPlan::InsertNotMatched {
                columns, values, ..
            } => {
                if columns.is_empty() {
                    for column in layer.columns.iter().take(values.len()) {
                        modified.push_copy(column.name.as_str())?;
                    }
                } else {
                    for target in columns {
                        modified.push_copy(target.column.as_str())?;
                    }
                }
            }
            MergeWhenPlan::UpdateMatched { assignments, .. }
            | MergeWhenPlan::UpdateNotMatchedBySource { assignments, .. } => {
                for name in assignments
                    .iter()
                    .flat_map(|assignment| assignment.target.column_names())
                {
                    modified.push_copy(name)?;
                }
            }
            _ => {}
        }
    }
    validation::writable_columns(layer, modified.iter().copied(), ColumnWrite::Merge, control)
}
