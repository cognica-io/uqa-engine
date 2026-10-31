//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{
    bind_source_plan_schema, LockStrength, PhysicalRow, QueryPlan, RelationalPlan, RowLockContext,
    RowProjectionValue, RowSchema, SQLError, SQLParam, SourcePlan,
};

use crate::query::CteScope;
use uqa_core::memory::ProductionControl;

mod sources;
pub(super) use sources::{collect_source_leaves, LockLeaf, LockLeafKind};

pub(super) fn collect_source_leaf_plans<'a>(
    source: &'a SourcePlan,
    path: &mut Vec<u8>,
    leaves: &mut Vec<(Vec<u8>, &'a SourcePlan)>,
) {
    match source {
        SourcePlan::Join { left, right, .. } => {
            path.push(0);
            collect_source_leaf_plans(left, path, leaves);
            path.pop();
            path.push(1);
            collect_source_leaf_plans(right, path, leaves);
            path.pop();
        }
        SourcePlan::RetainedRows(_)
        | SourcePlan::Table { .. }
        | SourcePlan::Values { .. }
        | SourcePlan::Function { .. }
        | SourcePlan::FunctionGroup { .. }
        | SourcePlan::Subquery { .. } => leaves.push((path.clone(), source)),
    }
}

#[cold]
#[inline(never)]
pub(super) fn copy_recheck_source_row<S: Clone + Send + Sync + 'static>(
    context: RowLockContext<'_, S>,
    source: &SourcePlan,
    qualifier: &str,
    candidate_schema: &RowSchema,
    candidate: &PhysicalRow,
    params: &[SQLParam],
    ctes: &CteScope<S>,
) -> Result<(RowSchema, PhysicalRow), SQLError> {
    if let SourcePlan::RetainedRows(source) = source {
        return copy_retained_source_row(source, candidate_schema, candidate);
    }
    let (schema, slots) = if qualifier.is_empty() {
        let source_schema = bind_source_plan_schema(context.catalog, source, params, ctes, None)?;
        let slots = source_schema
            .identities()
            .iter()
            .map(|identity| {
                candidate_schema
                    .physical_slot_for_identity(identity)
                    .ok_or_else(|| {
                        SQLError::Internal(format!(
                            "row-lock recheck cannot identify unqualified copy-row column `{}`",
                            identity.column()
                        ))
                    })
            })
            .collect::<Result<Vec<_>, _>>()?;
        (
            RowSchema::with_identities(
                source_schema.columns().to_vec(),
                source_schema.identities().to_vec(),
                source_schema.column_types().to_vec(),
            ),
            slots,
        )
    } else {
        let layout = candidate_schema.qualified_star_layout(qualifier);
        let columns = layout
            .iter()
            .map(|(column, _, _)| column.clone())
            .collect::<Vec<_>>();
        let types = layout
            .iter()
            .map(|(_, _, column_type)| column_type.clone())
            .collect::<Vec<_>>();
        let slots = layout
            .into_iter()
            .map(|(_, slot, _)| slot)
            .collect::<Vec<_>>();
        (
            RowSchema::with_qualified_types(qualifier, columns, types),
            slots,
        )
    };
    let row = candidate
        .project_with_values(slots.into_iter().map(RowProjectionValue::InputSlot))
        .without_lock_origins();
    Ok((schema, row))
}

fn copy_retained_source_row(
    source: &uqa_sql::ast::RetainedRowSource,
    candidate_schema: &RowSchema,
    candidate: &PhysicalRow,
) -> Result<(RowSchema, PhysicalRow), SQLError> {
    let available = candidate_schema.internal_columns_with_types();
    let slots = (0..source.schema().physical_width())
        .map(|position| {
            let column = source.relation().column(position);
            available
                .binary_search_by_key(&column, |(found, _, _)| *found)
                .ok()
                .map(|index| {
                    let (_, slot, _) = available[index];
                    slot.map_or_else(
                        || RowProjectionValue::Owned(uqa_core::Value::Null),
                        RowProjectionValue::InputSlot,
                    )
                })
                .ok_or_else(|| {
                    SQLError::Internal("row-lock recheck lost a retained source attribute".into())
                })
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok((
        source.schema().clone(),
        candidate.project_with_values(slots).without_lock_origins(),
    ))
}

pub(super) fn reject_unusable_lock_leaf<S: Clone + Send + Sync + 'static>(
    context: RowLockContext<'_, S>,
    source: &LockLeaf<'_>,
    strength: LockStrength,
    control: &ProductionControl<'_>,
) -> Result<(), SQLError> {
    control.check()?;
    match &source.kind {
        LockLeafKind::Values => Ok(()),
        LockLeafKind::Function => Err(SQLError::Unsupported(
            "FOR UPDATE/SHARE cannot be applied to a function".into(),
        )),
        LockLeafKind::Cte => Err(SQLError::Unsupported(format!(
            "{} cannot be applied to a WITH query",
            strength.sql_name()
        ))),
        LockLeafKind::Foreign => Err(SQLError::Unsupported(
            control
                .format(format_args!(
                    "{} cannot be applied to foreign table \"{}\"",
                    strength.sql_name(),
                    source.display_name
                ))?
                .into_parts()
                .0,
        )),
        LockLeafKind::Virtual { lockable: true } => {
            reject_nullable_lock_source(source.nullable, strength)
        }
        LockLeafKind::Virtual { lockable: false } => Err(SQLError::Unsupported(
            control
                .format(format_args!(
                    "{} cannot be applied to relation \"{}\"",
                    strength.sql_name(),
                    source.display_name
                ))?
                .into_parts()
                .0,
        )),
        LockLeafKind::Base => reject_nullable_lock_source(source.nullable, strength),
        LockLeafKind::View(plan) => {
            validate_view_locking(context, plan, strength, source.nullable, control)
        }
        LockLeafKind::Subquery(plan) => {
            validate_propagated_query(context, plan, strength, source.nullable, false, control)
        }
    }
}

pub(super) fn reject_nullable_lock_source(
    nullable: bool,
    strength: LockStrength,
) -> Result<(), SQLError> {
    if nullable {
        return Err(SQLError::Unsupported(format!(
            "{} cannot be applied to the nullable side of an outer join",
            strength.sql_name()
        )));
    }
    Ok(())
}

pub(super) fn validate_view_locking<S: Clone + Send + Sync + 'static>(
    context: RowLockContext<'_, S>,
    plan: &QueryPlan,
    strength: LockStrength,
    nullable: bool,
    control: &ProductionControl<'_>,
) -> Result<(), SQLError> {
    validate_propagated_query(context, plan, strength, nullable, true, control)
}

pub(super) fn validate_propagated_query<S: Clone + Send + Sync + 'static>(
    context: RowLockContext<'_, S>,
    plan: &QueryPlan,
    strength: LockStrength,
    nullable: bool,
    allow_set_operation_root: bool,
    control: &ProductionControl<'_>,
) -> Result<(), SQLError> {
    control.check()?;
    match &plan.root {
        RelationalPlan::SetOp { .. } if allow_set_operation_root => Ok(()),
        RelationalPlan::SetOp { .. } => Err(SQLError::Unsupported(format!(
            "{} is not allowed with UNION/INTERSECT/EXCEPT",
            strength.sql_name()
        ))),
        RelationalPlan::Values { .. } => Ok(()),
        RelationalPlan::QueryBlock(block) => {
            validate_locking_block_shape(block, strength)?;
            let Some(source) = block.from.as_ref() else {
                return Ok(());
            };
            validate_propagated_source(context, source, strength, nullable, plan, control)
        }
    }
}

pub(super) use uqa_sql::semantics::locking::validate_locking_block_shape;

pub(super) fn validate_propagated_source<S: Clone + Send + Sync + 'static>(
    context: RowLockContext<'_, S>,
    source: &SourcePlan,
    strength: LockStrength,
    nullable: bool,
    owner: &QueryPlan,
    control: &ProductionControl<'_>,
) -> Result<(), SQLError> {
    control.check()?;
    match source {
        SourcePlan::Table { name, .. } => {
            if owner.ctes.iter().any(|cte| cte.name == *name)
                || context.catalog.view_exists_with_control(name, control)?
            {
                return Ok(());
            }
            reject_nullable_lock_source(nullable, strength)
        }
        SourcePlan::Join {
            left, right, kind, ..
        } => {
            let (left_nullable, right_nullable) = join_nullability(*kind, nullable);
            validate_propagated_source(context, left, strength, left_nullable, owner, control)?;
            validate_propagated_source(context, right, strength, right_nullable, owner, control)
        }
        SourcePlan::Subquery { body, .. } => {
            validate_propagated_query(context, body, strength, nullable, false, control)
        }
        SourcePlan::RetainedRows(_)
        | SourcePlan::Values { .. }
        | SourcePlan::Function { .. }
        | SourcePlan::FunctionGroup { .. } => Ok(()),
    }
}

fn join_nullability(kind: uqa_sql::ast::JoinKind, nullable: bool) -> (bool, bool) {
    use uqa_sql::ast::JoinKind;
    match kind {
        JoinKind::Left => (nullable, true),
        JoinKind::Right => (true, nullable),
        JoinKind::Full => (true, true),
        JoinKind::Inner | JoinKind::Cross => (nullable, nullable),
    }
}

#[cfg(test)]
mod tests;
