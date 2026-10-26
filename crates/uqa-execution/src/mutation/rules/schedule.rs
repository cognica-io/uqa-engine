//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Rewrite-rule commands and the surviving original share statement resources, not evaluated event rows.

use crate::{
    mutation::{
        output::MutationOutput, statement::context::MutationStatementContext, statement_end,
        views::commands::SourceOutputPruning,
    },
    query::CteScope,
};
use uqa_sql::{
    ast::{RuleEvent, TriggerEvent},
    plan::{DeletePlan, UpdatePlan},
    semantics::{
        rules::binding::{MutationRuleInput, RuleMutation},
        view_privileges, view_rewrite,
    },
    SQLError, SQLParam,
};

enum MutationCommand {
    Update(UpdatePlan),
    Delete(DeletePlan),
}

impl MutationCommand {
    fn input(&self) -> RuleMutation<'_> {
        match self {
            Self::Update(plan) => RuleMutation::Update(plan),
            Self::Delete(plan) => RuleMutation::Delete(plan),
        }
    }

    fn clear_ctes(&mut self) {
        match self {
            Self::Update(plan) => plan.ctes.clear(),
            Self::Delete(plan) => plan.ctes.clear(),
        }
    }

    fn privilege_subject(&self) -> Option<&uqa_sql::catalog::roles::RoleReference> {
        match self {
            Self::Update(plan) => plan.statement_privilege_subject.as_ref(),
            Self::Delete(plan) => plan.statement_privilege_subject.as_ref(),
        }
    }

    fn relations_bound(&self) -> bool {
        match self {
            Self::Update(plan) => plan.relations_bound,
            Self::Delete(plan) => plan.relations_bound,
        }
    }
}

pub fn run_update<S: Clone + Send + Sync + 'static>(
    context: &MutationStatementContext<'_, S>,
    prune: SourceOutputPruning,
    statement: &UpdatePlan,
    params: &[SQLParam],
    inherited: Option<&CteScope<S>>,
) -> Result<MutationOutput, SQLError> {
    if plain_table(context, RuleMutation::Update(statement))? {
        return crate::mutation::update::table::run_table_update(
            context, statement, params, inherited,
        );
    }
    run(
        context,
        prune,
        MutationCommand::Update(statement.clone()),
        params,
        inherited,
    )
}

pub fn run_delete<S: Clone + Send + Sync + 'static>(
    context: &MutationStatementContext<'_, S>,
    prune: SourceOutputPruning,
    statement: &DeletePlan,
    params: &[SQLParam],
    inherited: Option<&CteScope<S>>,
) -> Result<MutationOutput, SQLError> {
    if plain_table(context, RuleMutation::Delete(statement))? {
        return crate::mutation::delete::run_table_delete(context, statement, params, inherited);
    }
    run(
        context,
        prune,
        MutationCommand::Delete(statement.clone()),
        params,
        inherited,
    )
}

fn run<S: Clone + Send + Sync + 'static>(
    context: &MutationStatementContext<'_, S>,
    prune: SourceOutputPruning,
    statement: MutationCommand,
    params: &[SQLParam],
    inherited: Option<&CteScope<S>>,
) -> Result<MutationOutput, SQLError> {
    let (mut original, relations) = prepare(context, &statement, params, inherited)?;
    if relations.is_empty() {
        return original.map_or_else(
            || Ok(MutationOutput::from_affected(0)),
            |original| run_original(context, prune, &original, params, inherited),
        );
    }
    let (commands, _running) = statement_end::statement_commands(inherited);
    let mut scope = context
        .mutation
        .scopes
        .command_scope(statement.privilege_subject(), statement.relations_bound())?;
    if let Some(parent) = inherited {
        scope.inherit_cte_bindings(parent);
    }
    scope.set_statement_commands(commands);
    let input = statement.input();
    let result = (|| {
        crate::query::cte::materialize_command_ctes(
            context.query.source.ctes,
            input.ctes(),
            || {
                uqa_sql::semantics::primary_command_cte_references(
                    input.ctes(),
                    input.subqueries().iter(),
                    input.source(),
                )
            },
            params,
            &mut scope,
        )?;
        let mut output = None;
        let mut affected = 0;
        for relation in &relations {
            let input = relation.input();
            scope.scalar_subqueries = input.subqueries().into();
            let source_schema = context
                .mutation
                .rules
                .rules
                .source_schema(input, params, &scope)?;
            let mut projection = match relation {
                MutationCommand::Update(plan) => super::RuleOutputProjection::update(
                    context.mutation.preparation.returning,
                    plan,
                    params,
                    &scope,
                ),
                MutationCommand::Delete(plan) => super::RuleOutputProjection::delete(
                    context.mutation.preparation.returning,
                    plan,
                    params,
                    &scope,
                ),
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
        if let Some(original) = &mut original {
            original.clear_ctes();
            let result = run_original(context, prune, original, params, Some(&scope))?;
            if output.is_none() {
                return Ok(result);
            }
        }
        Ok(output.unwrap_or_else(|| MutationOutput::from_affected(affected)))
    })();
    statement_end::finish_statement(context, params, input.ctes(), Some(&mut scope), result)
}

fn prepare<S: Clone + Send + Sync + 'static>(
    context: &MutationStatementContext<'_, S>,
    statement: &MutationCommand,
    params: &[SQLParam],
    inherited: Option<&CteScope<S>>,
) -> Result<(Option<MutationCommand>, Vec<MutationCommand>), SQLError> {
    let rewrite = context.mutation.rules.views;
    let rule_context = &context.mutation.rules.rules;
    let input = statement.input();
    let kind = relation_kind(context, statement)?;
    let trigger = match input.event() {
        RuleEvent::Update => TriggerEvent::Update,
        RuleEvent::Delete => TriggerEvent::Delete,
        _ => unreachable!("mutation event"),
    };
    let automatic =
        kind.is_some() && !view_rewrite::has_instead_of_trigger(rewrite, input.table(), trigger)?;
    let bindings = inherited
        .map(crate::query::binding::binding_context)
        .transpose()?
        .map(uqa_sql::binding::snapshot::BindingSnapshot::from);
    let mut relations = Vec::new();
    let original = match statement {
        MutationCommand::Update(plan) => {
            let mut visit = |plan: &mut UpdatePlan| -> Result<bool, SQLError> {
                validate_update(context, plan)?;
                let rules = rule_context.analysis;
                if rules
                    .rules
                    .rules_for(&plan.table, RuleEvent::Update)?
                    .is_empty()
                {
                    return Ok(true);
                }
                relations.push(MutationCommand::Update(plan.clone()));
                let subject = rule_context.security.privilege_subject(&plan.table)?;
                MutationRuleInput::qualify_update_original(
                    rules,
                    plan,
                    rule_context.binding,
                    &subject,
                )
            };
            let mut original = if automatic {
                view_rewrite::rewrite_update_rule_inputs(
                    rewrite,
                    plan,
                    params,
                    bindings.as_ref(),
                    &mut visit,
                )?
            } else {
                Some(plan.clone())
            };
            if let Some(plan) = &mut original {
                if !visit(plan)? {
                    original = None;
                }
            }
            original.map(MutationCommand::Update)
        }
        MutationCommand::Delete(plan) => {
            let mut visit = |plan: &mut DeletePlan| -> Result<bool, SQLError> {
                validate_delete(context, plan)?;
                let rules = rule_context.analysis;
                if rules
                    .rules
                    .rules_for(&plan.table, RuleEvent::Delete)?
                    .is_empty()
                {
                    return Ok(true);
                }
                relations.push(MutationCommand::Delete(plan.clone()));
                let subject = rule_context.security.privilege_subject(&plan.table)?;
                MutationRuleInput::qualify_delete_original(
                    rules,
                    plan,
                    rule_context.binding,
                    &subject,
                )
            };
            let mut original = if automatic {
                view_rewrite::rewrite_delete_rule_inputs(
                    rewrite,
                    plan,
                    params,
                    bindings.as_ref(),
                    &mut visit,
                )?
            } else {
                Some(plan.clone())
            };
            if let Some(plan) = &mut original {
                if !visit(plan)? {
                    original = None;
                }
            }
            original.map(MutationCommand::Delete)
        }
    };
    Ok((original, relations))
}

fn validate_update<S: Clone + Send + Sync + 'static>(
    context: &MutationStatementContext<'_, S>,
    plan: &UpdatePlan,
) -> Result<(), SQLError> {
    lock_and_validate_returning(context, RuleMutation::Update(plan))?;
    if context
        .mutation
        .rules
        .views
        .catalog
        .target_view_kind(&plan.table)?
        .is_some()
    {
        view_privileges::ensure_update(context.mutation.rules.views.authorization, plan)?;
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
) -> Result<(), SQLError> {
    lock_and_validate_returning(context, RuleMutation::Delete(plan))?;
    if context
        .mutation
        .rules
        .views
        .catalog
        .target_view_kind(&plan.table)?
        .is_some()
    {
        view_privileges::ensure_delete(context.mutation.rules.views.authorization, plan)?;
    } else {
        uqa_sql::semantics::mutation_privileges::ensure_delete_target_privileges(
            context.mutation.privileges,
            plan,
        )?;
    }
    Ok(())
}

fn lock_and_validate_returning<S: Clone + Send + Sync + 'static>(
    context: &MutationStatementContext<'_, S>,
    input: RuleMutation<'_>,
) -> Result<(), SQLError> {
    context.query.source.locking.session.lock_relation(
        input.table(),
        crate::row_locks::RelationLockMode::RowExclusive,
    )?;
    uqa_sql::semantics::rules::validate_rule_returning_contract(
        context.mutation.rules.rules.analysis.rules,
        input.table(),
        input.event(),
        !input.returning().is_empty(),
    )
}

fn run_original<S: Clone + Send + Sync + 'static>(
    context: &MutationStatementContext<'_, S>,
    prune: SourceOutputPruning,
    original: &MutationCommand,
    params: &[SQLParam],
    inherited: Option<&CteScope<S>>,
) -> Result<MutationOutput, SQLError> {
    let view = context
        .mutation
        .rules
        .views
        .catalog
        .target_view_kind(original.input().table())?
        .is_some();
    match original {
        MutationCommand::Update(plan) if view => {
            crate::mutation::routine_calls::update(context, plan, params, inherited)?;
            crate::mutation::views::commands::run_view_update_inner(
                context, prune, plan, params, inherited,
            )
        }
        MutationCommand::Delete(plan) if view => {
            crate::mutation::routine_calls::delete(context, plan, params, inherited)?;
            crate::mutation::views::commands::run_view_delete_inner(
                context, prune, plan, params, inherited,
            )
        }
        MutationCommand::Update(plan) => {
            crate::mutation::update::table::run_table_update(context, plan, params, inherited)
        }
        MutationCommand::Delete(plan) => {
            crate::mutation::delete::run_table_delete(context, plan, params, inherited)
        }
    }
}

fn relation_kind<S: Clone + Send + Sync + 'static>(
    context: &MutationStatementContext<'_, S>,
    statement: &MutationCommand,
) -> Result<Option<uqa_sql::catalog::view::StoredViewKind>, SQLError> {
    let rewrite = context.mutation.rules.views;
    let input = statement.input();
    let kind = rewrite.catalog.target_view_kind(input.table())?;
    if kind == Some(uqa_sql::catalog::view::StoredViewKind::Materialized) {
        match statement {
            MutationCommand::Update(plan) => {
                view_privileges::ensure_update(rewrite.authorization, plan)?;
            }
            MutationCommand::Delete(plan) => {
                view_privileges::ensure_delete(rewrite.authorization, plan)?;
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

fn plain_table<S: Clone + 'static>(
    context: &MutationStatementContext<'_, S>,
    input: RuleMutation<'_>,
) -> Result<bool, SQLError> {
    Ok(context
        .mutation
        .rules
        .views
        .catalog
        .target_view_kind(input.table())?
        .is_none()
        && context
            .mutation
            .rules
            .rules
            .analysis
            .rules
            .rules_for(input.table(), input.event())?
            .is_empty())
}
