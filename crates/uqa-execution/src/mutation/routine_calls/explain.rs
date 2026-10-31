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
    let (target, bound, include_descendants) = match command {
        CommandPlan::Insert(plan) => (
            &plan.table,
            plan.target_relation_bound,
            plan.include_descendants,
        ),
        CommandPlan::Update(plan) => (
            &plan.table,
            plan.target_relation_bound,
            plan.include_descendants,
        ),
        CommandPlan::Delete(plan) => (
            &plan.table,
            plan.target_relation_bound,
            plan.include_descendants,
        ),
        CommandPlan::Merge(plan) => (&plan.target, false, plan.include_descendants),
        _ => return Ok(()),
    };
    let mutation = &context.mutation;
    let retained = mutation.scopes.read_control()?;
    let control = uqa_core::memory::ProductionControl::new(
        retained.memory(),
        retained.cancellation(),
        mutation.scopes.cancellation(),
    );
    // EXPLAIN resolves the same namespace before borrowing bound catalog services and keeps its private command owner through initialization.
    let command = command.copy_for_rewrite(&control)?;
    let target = targets.resolve_target_with_control(target, bound, &control)?;
    let command = command
        .retarget_mutation(target, include_descendants, &control)?
        .finish(&control)?;
    let target = command
        .mutation_target()
        .expect("selected mutation command");
    let view = mutation
        .rules
        .views
        .catalog
        .target_view_kind_with_control(target, &control)?
        .is_some();
    initialize_command(context, &command, params, view, &control)
}

fn initialize_command<S: Clone + Send + Sync + 'static>(
    context: &MutationStatementContext<'_, S>,
    command: &CommandPlan,
    params: &[SQLParam],
    view: bool,
    control: &uqa_core::memory::ProductionControl<'_>,
) -> Result<(), SQLError> {
    let mutation = &context.mutation;
    match command {
        CommandPlan::Insert(plan) => {
            if view {
                view_privileges::ensure_insert_with_control(
                    mutation.rules.views.authorization,
                    plan,
                    control,
                )?;
            } else {
                let columns =
                    mutation_privileges::conflict_update_columns_with_control(plan, control)?;
                mutation_privileges::ensure_insert_target_privileges_with_control(
                    mutation.privileges,
                    plan,
                    columns.as_ref().map(|columns| columns.as_slice()),
                    control,
                )?;
            }
            super::insert(context, plan, params, None)
        }
        CommandPlan::Update(plan) => {
            if view {
                view_privileges::ensure_update_with_control(
                    mutation.rules.views.authorization,
                    plan,
                    control,
                )?;
            } else {
                mutation_privileges::ensure_update_target_privileges_with_control(
                    mutation.privileges,
                    plan,
                    control,
                )?;
            }
            super::update(context, plan, params, None)
        }
        CommandPlan::Delete(plan) => {
            if view {
                view_privileges::ensure_delete_with_control(
                    mutation.rules.views.authorization,
                    plan,
                    control,
                )?;
            } else {
                mutation_privileges::ensure_delete_target_privileges_with_control(
                    mutation.privileges,
                    plan,
                    control,
                )?;
            }
            super::delete(context, plan, params, None)
        }
        CommandPlan::Merge(plan) => {
            if view {
                view_privileges::ensure_merge_with_control(
                    mutation.rules.views.authorization,
                    plan,
                    control,
                )?;
            } else {
                super::super::merge::analysis::ensure_merge_privileges(mutation, plan, None)?;
            }
            super::merge(context, plan, params, None)
        }
        _ => Ok(()),
    }
}
