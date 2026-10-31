//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Admit row-lock target syntax, selection scratch and independently retained resolved names.

use super::{
    leaf_validation::{collect_source_leaves, reject_unusable_lock_leaf, LockLeaf},
    locks_non_temporary_relation, merge_lock_wait, validate_lock_relation_aliases, CteScope,
    LockStrength, LockWait, LockingClause, ResolvedRowLock, RowLockContext, SQLError, SQLParam,
    ScalarExpr, SourcePlan,
};
use uqa_core::memory::{Produced, ProductionControl, ProductionVec};

type LockAssignment = Option<(LockStrength, LockWait)>;

pub fn resolve_row_locks<S: Clone + Send + Sync + 'static>(
    context: RowLockContext<'_, S>,
    from: &SourcePlan,
    locking: &[LockingClause],
    predicate: Option<&ScalarExpr>,
    params: &[SQLParam],
    ctes: &CteScope<S>,
) -> Result<Produced<Vec<ResolvedRowLock>>, SQLError> {
    let resources = context.scopes.production_resources()?;
    let control = resources.control();
    control.check()?;
    let mut resolved = ProductionVec::new(control);
    if locking.is_empty() {
        return Ok(resolved.finish()?);
    }
    let effective_from = super::null_rejection::reduce_null_rejected_outer_joins(
        context,
        from.copy_for_rewrite(&control)?,
        predicate,
        params,
        ctes,
        &control,
    )?;
    validate_lock_relation_aliases(&effective_from, locking)?;
    let catalog = ctes.catalog_read_view()?;
    let resolution = ctes.relation_name_resolution_with_control(&control)?;
    let sources = collect_source_leaves(
        &effective_from,
        false,
        &catalog,
        &resolution,
        ctes,
        &control,
    )?;
    let assigned = select_targets(&sources, locking, &control)?;
    for (source, assignment) in sources.iter().zip(assigned.iter()) {
        let Some((strength, wait)) = *assignment else {
            continue;
        };
        reject_unusable_lock_leaf(context, source, strength, &control)?;
        if source.kind.carries_row_identity() {
            resolved.push_produced(retain_target(source, strength, wait, &control)?)?;
        }
    }
    if context.session.current_transaction_is_read_only()
        && locks_non_temporary_relation(context, &resolved)?
    {
        return Err(SQLError::Routine {
            sqlstate: "25006".into(),
            message: "cannot execute SELECT in a read-only transaction".into(),
        });
    }
    Ok(resolved.finish()?)
}

fn select_targets(
    sources: &[LockLeaf<'_>],
    locking: &[LockingClause],
    control: &ProductionControl<'_>,
) -> Result<Produced<Vec<LockAssignment>>, SQLError> {
    let mut assigned: ProductionVec<'_, LockAssignment> = ProductionVec::new(*control);
    for _ in sources {
        assigned.push_copy(None)?;
    }
    for clause in locking {
        let mut selected = ProductionVec::new(*control);
        for source in sources {
            selected.push_copy(clause.relations.is_empty() && source.kind.implicitly_lockable())?;
        }
        for relation in &clause.relations {
            let mut found = false;
            for (index, source) in sources.iter().enumerate() {
                control.check()?;
                if source.names.iter().flatten().any(|name| *name == relation) {
                    selected.as_mut_slice()[index] = true;
                    found = true;
                }
            }
            if !found {
                return Err(SQLError::Routine { sqlstate: "42P01".into(), message: control.format(format_args!("relation \"{relation}\" in FOR UPDATE/SHARE clause not found in FROM clause"))?.into_parts().0 });
            }
        }
        for (index, selected) in selected.iter().enumerate() {
            if *selected {
                assigned.as_mut_slice()[index] = Some(match assigned[index] {
                    Some((strength, wait)) => (
                        strength.max(clause.strength),
                        merge_lock_wait(wait, clause.wait),
                    ),
                    None => (clause.strength, clause.wait),
                });
            }
        }
    }
    Ok(assigned.finish()?)
}

fn retain_target(
    source: &LockLeaf<'_>,
    strength: LockStrength,
    wait: LockWait,
    control: &ProductionControl<'_>,
) -> Result<Produced<ResolvedRowLock>, SQLError> {
    let qualifier = control.copy_text(source.qualifier)?;
    let storage_name = control.copy_text(&source.storage_name)?;
    let display_name = control.copy_text(source.display_name)?;
    let (qualifier, first) = qualifier.into_parts();
    let (storage_name, second) = storage_name.into_parts();
    let (display_name, third) = display_name.into_parts();
    Ok(control.finish(
        ResolvedRowLock {
            qualifier,
            storage_name,
            display_name,
            strength,
            wait,
            identity_source: source.kind.is_identity_source(),
        },
        control.combine(control.combine(first, second), third),
    )?)
}

#[cfg(test)]
mod tests;
