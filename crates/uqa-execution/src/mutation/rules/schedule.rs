//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Rewrite-rule commands and the surviving original retain their own admitted syntax through execution.

use crate::{
    mutation::{
        output::MutationOutput, statement::context::MutationStatementContext, statement_end,
    },
    query::CteScope,
};
use uqa_core::memory::{Produced, ProductionControl, ProductionVec};
use uqa_sql::{
    ast::{RuleEvent, TriggerEvent, TriggerTiming},
    plan::{CommandPlan, DeletePlan, MutableCommandPlan, UpdatePlan},
    semantics::{
        rules::binding::{OwnedMutationRuleInput, RuleMutation},
        view_privileges, view_rewrite,
    },
    SQLError, SQLParam,
};

type PreparedMutation = (
    Option<MutableCommandPlan>,
    Produced<Vec<MutableCommandPlan>>,
);

fn mutation(command: &CommandPlan) -> RuleMutation<'_> {
    match command {
        CommandPlan::Update(plan) => RuleMutation::Update(plan),
        CommandPlan::Delete(plan) => RuleMutation::Delete(plan),
        _ => unreachable!("UPDATE/DELETE command schedule"),
    }
}

pub fn run_update<S: Clone + Send + Sync + 'static>(
    context: &MutationStatementContext<'_, S>,
    statement: &UpdatePlan,
    params: &[SQLParam],
    inherited: Option<&CteScope<S>>,
) -> Result<MutationOutput, SQLError> {
    let resources = context.mutation.scopes.read_control()?;
    let control = ProductionControl::new(
        resources.memory(),
        resources.cancellation(),
        context.mutation.scopes.cancellation(),
    );
    let input = RuleMutation::Update(statement);
    if plain_table(context, input, &control)? {
        return crate::mutation::update::table::run_table_update(
            context, statement, params, inherited,
        );
    }
    run(context, input, params, inherited, &control)
}

pub fn run_delete<S: Clone + Send + Sync + 'static>(
    context: &MutationStatementContext<'_, S>,
    statement: &DeletePlan,
    params: &[SQLParam],
    inherited: Option<&CteScope<S>>,
) -> Result<MutationOutput, SQLError> {
    let resources = context.mutation.scopes.read_control()?;
    let control = ProductionControl::new(
        resources.memory(),
        resources.cancellation(),
        context.mutation.scopes.cancellation(),
    );
    let input = RuleMutation::Delete(statement);
    if plain_table(context, input, &control)? {
        return crate::mutation::delete::run_table_delete(context, statement, params, inherited);
    }
    run(context, input, params, inherited, &control)
}

fn run<S: Clone + Send + Sync + 'static>(
    context: &MutationStatementContext<'_, S>,
    statement: RuleMutation<'_>,
    params: &[SQLParam],
    inherited: Option<&CteScope<S>>,
    control: &ProductionControl<'_>,
) -> Result<MutationOutput, SQLError> {
    let (mut original, relations) = prepare(context, statement, params, inherited, control)?;
    if relations.is_empty() {
        return original.map_or_else(
            || Ok(MutationOutput::from_affected(0)),
            |original| run_original(context, &original, params, inherited, control),
        );
    }
    let (commands, _running) = statement_end::statement_commands(inherited);
    let (subject, relations_bound) = match statement {
        RuleMutation::Update(plan) => (
            plan.statement_privilege_subject.as_ref(),
            plan.relations_bound,
        ),
        RuleMutation::Delete(plan) => (
            plan.statement_privilege_subject.as_ref(),
            plan.relations_bound,
        ),
    };
    let mut scope = context
        .mutation
        .scopes
        .command_scope(subject, relations_bound)?;
    if let Some(parent) = inherited {
        scope.inherit_cte_bindings(parent);
    }
    scope.set_statement_commands(commands);
    let result = (|| {
        crate::query::cte::materialize_command_ctes(
            context.query.source.ctes,
            statement.ctes(),
            |control| {
                uqa_sql::semantics::primary_command_cte_references_with_control(
                    statement.ctes(),
                    statement.subqueries().iter(),
                    statement.source(),
                    control,
                )
            },
            params,
            &mut scope,
        )?;
        let mut output = None;
        let mut affected = 0;
        for relation in relations.iter() {
            control.check()?;
            let input = mutation(relation);
            scope.scalar_subqueries =
                uqa_sql::plan::RetainedQueryArena::capture(input.subqueries(), control)?;
            let source_schema = context
                .mutation
                .rules
                .rules
                .source_schema(input, params, &scope)?;
            let mut projection = match &**relation {
                CommandPlan::Update(plan) => super::RuleOutputProjection::update(
                    context.mutation.preparation.returning,
                    plan,
                    params,
                    &scope,
                ),
                CommandPlan::Delete(plan) => super::RuleOutputProjection::delete(
                    context.mutation.preparation.returning,
                    plan,
                    params,
                    &scope,
                ),
                _ => unreachable!("scheduled mutation"),
            };
            projection.shape.supplemental_schema = source_schema.as_ref();
            let outcome = super::execute_mutation_query_rules(
                &context.mutation.rules.rules.with_projection(&projection),
                input,
                params,
                &scope,
            )?;
            if outcome.sets_command_tag {
                affected = outcome.affected_rows;
            }
            if let Some(returning) = outcome.returning {
                if output.is_some() {
                    return Err(SQLError::Routine {
                        sqlstate: "0A000".into(),
                        message: "cannot have RETURNING lists in multiple rules".into(),
                    });
                }
                output = Some(
                    returning.project(context.mutation.preparation.returning, projection.shape)?,
                );
            }
        }
        if let Some(original) = original.take() {
            let original = original.clear_mutation_ctes(control)?;
            let result = run_original(context, &original, params, Some(&scope), control)?;
            if output.is_none() {
                return Ok(result);
            }
        }
        Ok(output.unwrap_or_else(|| MutationOutput::from_affected(affected)))
    })();
    statement_end::finish_statement(context, params, statement.ctes(), Some(&mut scope), result)
}

fn prepare<S: Clone + Send + Sync + 'static>(
    context: &MutationStatementContext<'_, S>,
    statement: RuleMutation<'_>,
    params: &[SQLParam],
    inherited: Option<&CteScope<S>>,
    control: &ProductionControl<'_>,
) -> Result<PreparedMutation, SQLError> {
    let rewrite = context.mutation.rules.views;
    let rules = &context.mutation.rules.rules;
    let kind = relation_kind(context, statement, control)?;
    let trigger = match statement.event() {
        RuleEvent::Update => TriggerEvent::Update,
        RuleEvent::Delete => TriggerEvent::Delete,
        _ => unreachable!("mutation event"),
    };
    let automatic = kind.is_some()
        && !rewrite.catalog.has_trigger_definition_with_control(
            statement.table(),
            TriggerTiming::InsteadOf,
            trigger,
            true,
            control,
        )?;
    let bindings = inherited
        .map(|scope| crate::query::binding::binding_snapshot_with_control(scope, control))
        .transpose()?;
    let mut relations = ProductionVec::new(*control);
    let mut visit = |plan: MutableCommandPlan| -> Result<Option<MutableCommandPlan>, SQLError> {
        let input = mutation(&plan);
        lock_and_validate_returning(context, input, control)?;
        if rules
            .capture_rules(input.table(), input.event())?
            .is_empty()?
        {
            return Ok(Some(plan));
        }
        let snapshot = plan.copy_for_rewrite(control)?;
        relations.push_produced(control.finish(snapshot, control.empty_reservation())?)?;
        let subject = rules
            .security
            .privilege_subject_with_control(input.table(), control)?;
        OwnedMutationRuleInput::qualify_original(
            rules.analysis,
            plan,
            rules.binding,
            &subject,
            control,
        )
    };
    let input = match statement {
        RuleMutation::Update(plan) => plan.copy_for_rewrite(control)?,
        RuleMutation::Delete(plan) => plan.copy_for_rewrite(control)?,
    };
    let mut original = if automatic {
        view_rewrite::rewrite_rule_inputs_owned(
            rewrite,
            input,
            params,
            bindings
                .as_ref()
                .map(view_rewrite::ViewRewriteBindings::Controlled),
            &mut visit,
            control,
        )?
    } else {
        Some(input)
    };
    if let Some(plan) = original.take() {
        match &*plan {
            CommandPlan::Update(plan) => validate_update(context, plan, control)?,
            CommandPlan::Delete(plan) => validate_delete(context, plan, control)?,
            _ => unreachable!("surviving mutation"),
        }
        original = visit(plan)?;
    }
    Ok((original, relations.finish()?))
}

fn lock_and_validate_returning<S: Clone + Send + Sync + 'static>(
    context: &MutationStatementContext<'_, S>,
    input: RuleMutation<'_>,
    control: &ProductionControl<'_>,
) -> Result<(), SQLError> {
    context.query.source.locking.session.lock_relation(
        input.table(),
        crate::row_locks::RelationLockMode::RowExclusive,
    )?;
    uqa_sql::semantics::rules::validate_rule_returning_contract_with_control(
        context.mutation.rules.rules.analysis.rules,
        input.table(),
        input.event(),
        !input.returning().is_empty(),
        control,
    )
}

fn run_original<S: Clone + Send + Sync + 'static>(
    context: &MutationStatementContext<'_, S>,
    original: &MutableCommandPlan,
    params: &[SQLParam],
    inherited: Option<&CteScope<S>>,
    control: &ProductionControl<'_>,
) -> Result<MutationOutput, SQLError> {
    let view = context
        .mutation
        .rules
        .views
        .catalog
        .target_view_kind_with_control(mutation(original).table(), control)?
        .is_some();
    match &**original {
        CommandPlan::Update(plan) if view => {
            crate::mutation::routine_calls::update(context, plan, params, inherited)?;
            crate::mutation::views::commands::run_view_update_inner(
                context, plan, params, inherited,
            )
        }
        CommandPlan::Delete(plan) if view => {
            crate::mutation::routine_calls::delete(context, plan, params, inherited)?;
            crate::mutation::views::commands::run_view_delete_inner(
                context, plan, params, inherited,
            )
        }
        CommandPlan::Update(plan) => {
            crate::mutation::update::table::run_table_update(context, plan, params, inherited)
        }
        CommandPlan::Delete(plan) => {
            crate::mutation::delete::run_table_delete(context, plan, params, inherited)
        }
        _ => unreachable!("surviving mutation"),
    }
}

fn relation_kind<S: Clone + Send + Sync + 'static>(
    context: &MutationStatementContext<'_, S>,
    input: RuleMutation<'_>,
    control: &ProductionControl<'_>,
) -> Result<Option<uqa_sql::catalog::view::StoredViewKind>, SQLError> {
    let rewrite = context.mutation.rules.views;
    let kind = rewrite
        .catalog
        .target_view_kind_with_control(input.table(), control)?;
    if kind == Some(uqa_sql::catalog::view::StoredViewKind::Materialized) {
        match input {
            RuleMutation::Update(plan) => {
                view_privileges::ensure_update_with_control(rewrite.authorization, plan, control)?;
            }
            RuleMutation::Delete(plan) => {
                view_privileges::ensure_delete_with_control(rewrite.authorization, plan, control)?;
            }
        }
        let relation = uqa_core::RelationIdentity::from_legacy_name(input.table())
            .map_err(SQLError::Internal)?;
        return Err(SQLError::Routine {
            sqlstate: "42809".into(),
            message: format!("cannot change materialized view \"{}\"", relation.name),
        });
    }
    Ok(kind)
}

fn validate_update<S: Clone + Send + Sync + 'static>(
    context: &MutationStatementContext<'_, S>,
    plan: &UpdatePlan,
    control: &ProductionControl<'_>,
) -> Result<(), SQLError> {
    if context
        .mutation
        .rules
        .views
        .catalog
        .target_view_kind_with_control(&plan.table, control)?
        .is_some()
    {
        view_privileges::ensure_update_with_control(
            context.mutation.rules.views.authorization,
            plan,
            control,
        )?;
    } else {
        uqa_sql::semantics::mutation_privileges::ensure_update_target_privileges(
            context.mutation.privileges,
            plan,
        )?;
        if plan.view_rule_update_plans.is_empty() {
            let columns = context.mutation.preparation.referential.assignment.columns;
            uqa_sql::assignment::columns::validate_mutation_targets(
                columns,
                &plan.table,
                plan.assignments
                    .iter()
                    .flat_map(|assignment| assignment.target.targets()),
                "UPDATE",
                false,
            )?;
            uqa_sql::semantics::generated_values::GeneratedValueColumns::of(columns, &plan.table)?
                .validate_update(plan.assignments.iter().flat_map(|assignment| {
                    assignment.target.targets().iter().map(move |target| {
                        (
                            target.column.as_str(),
                            matches!(assignment.value, uqa_sql::ScalarExpr::Default),
                        )
                    })
                }))?;
        }
    }
    Ok(())
}

fn validate_delete<S: Clone + Send + Sync + 'static>(
    context: &MutationStatementContext<'_, S>,
    plan: &DeletePlan,
    control: &ProductionControl<'_>,
) -> Result<(), SQLError> {
    if context
        .mutation
        .rules
        .views
        .catalog
        .target_view_kind_with_control(&plan.table, control)?
        .is_some()
    {
        view_privileges::ensure_delete_with_control(
            context.mutation.rules.views.authorization,
            plan,
            control,
        )?;
    } else {
        uqa_sql::semantics::mutation_privileges::ensure_delete_target_privileges(
            context.mutation.privileges,
            plan,
        )?;
    }
    Ok(())
}

fn plain_table<S: Clone + 'static>(
    context: &MutationStatementContext<'_, S>,
    input: RuleMutation<'_>,
    control: &ProductionControl<'_>,
) -> Result<bool, SQLError> {
    Ok(context
        .mutation
        .rules
        .views
        .catalog
        .target_view_kind_with_control(input.table(), control)?
        .is_none()
        && context
            .mutation
            .rules
            .rules
            .capture_rules(input.table(), input.event())?
            .is_empty()?)
}
