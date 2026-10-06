//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! EXPLAIN initializes mutation expressions without dispatching statement or row effects.

use super::{BuiltinRoutinePermissions, MutationStatementContext};
use uqa_sql::{
    plan::CommandPlan,
    semantics::{mutation_privileges, view_privileges},
    SQLError, SQLParam,
};

pub fn explain<S: Clone + Send + Sync + 'static>(
    context: &MutationStatementContext<'_, S>,
    targets: &dyn crate::mutation::entry::MutationTargetResolution,
    command: &CommandPlan,
    params: &[SQLParam],
) -> Result<(), SQLError> {
    if BuiltinRoutinePermissions::capture(&context.query.source.catalog).unrestricted() {
        return Ok(());
    }
    // EXPLAIN bypasses command entry; resolve its target through the same namespace adapter before borrowing bound catalog services.
    let mut command = command.clone();
    let (target, bound) = match &mut command {
        CommandPlan::Insert(plan) => (&mut plan.table, plan.target_relation_bound),
        CommandPlan::Update(plan) => (&mut plan.table, plan.target_relation_bound),
        CommandPlan::Delete(plan) => (&mut plan.table, plan.target_relation_bound),
        CommandPlan::Merge(plan) => (&mut plan.target, false),
        _ => return Ok(()),
    };
    *target = targets.resolve_target(target, bound)?;
    let mutation = &context.mutation;
    let Some(target) = command.mutation_target() else {
        return Ok(());
    };
    let view = mutation
        .rules
        .views
        .rewrite
        .catalog
        .target_view_kind(target)?
        .is_some();
    match &command {
        CommandPlan::Insert(plan) => {
            if view {
                view_privileges::ensure_insert(mutation.rules.views.rewrite.authorization, plan)?;
            } else {
                let columns =
                    plan.on_conflict
                        .as_ref()
                        .and_then(|conflict| match &conflict.action {
                            uqa_sql::plan::ConflictActionPlan::Update { assignments, .. } => Some(
                                assignments
                                    .iter()
                                    .flat_map(|assignment| assignment.target.column_names())
                                    .map(str::to_owned)
                                    .collect::<Vec<_>>(),
                            ),
                            uqa_sql::plan::ConflictActionPlan::Nothing => None,
                        });
                mutation_privileges::ensure_insert_target_privileges(
                    mutation.privileges,
                    plan,
                    columns.as_deref(),
                )?;
            }
            super::insert(context, plan, params, None)
        }
        CommandPlan::Update(plan) => {
            if view {
                view_privileges::ensure_update(mutation.rules.views.rewrite.authorization, plan)?;
            } else {
                mutation_privileges::ensure_update_target_privileges(mutation.privileges, plan)?;
            }
            super::update(context, plan, params, None)
        }
        CommandPlan::Delete(plan) => {
            if view {
                view_privileges::ensure_delete(mutation.rules.views.rewrite.authorization, plan)?;
            } else {
                mutation_privileges::ensure_delete_target_privileges(mutation.privileges, plan)?;
            }
            super::delete(context, plan, params, None)
        }
        CommandPlan::Merge(plan) => {
            if view {
                view_privileges::ensure_merge(mutation.rules.views.rewrite.authorization, plan)?;
            } else {
                super::super::merge::analysis::ensure_merge_privileges(mutation, plan, None)?;
            }
            super::merge(context, plan, params, None)
        }
        _ => Ok(()),
    }
}
