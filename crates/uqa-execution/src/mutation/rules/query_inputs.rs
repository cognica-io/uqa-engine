//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Execute each rewritten action with its own unevaluated INSERT query input.

use super::{RuleContext, RuleExecutionGuard, RuleExecutionOutcome, RuleReturningRequest};
use crate::{
    mutation::returning::ReturningRows, query::consumer::QueryRowConsumer,
    statement::plan_executor::StatementOutput,
};
use std::rc::Rc;
use uqa_core::memory::ProductionControl;
use uqa_sql::{
    ast::{InternalRelationId, RuleEvent, Statement},
    plan::InsertPlan,
    semantics::rules::{
        action_binding::{
            rule_action_target_columns_with_control, ActionColumns, BoundRuleStatement,
        },
        binding::{OwnedRuleQueryInput, OwnedRuleQueryResolver, RuleInputColumns},
        returning::validate_rule_returning_provider_width,
        statement_has_returning,
    },
    SQLError, SQLParam,
};

fn execute_insert_query_rules<S: Clone + 'static>(
    context: &RuleContext<'_, S>,
    insert: &InsertPlan,
    params: &[SQLParam],
    scope: &crate::query::CteScope<S>,
    prune: crate::mutation::views::commands::SourceOutputPruning,
) -> Result<RuleExecutionOutcome, SQLError> {
    let rules = context.capture_rules(&insert.table, RuleEvent::Insert)?;
    let _guard = RuleExecutionGuard::enter(&insert.table, RuleEvent::Insert)?;
    let privilege_subject = context.security.privilege_subject(&insert.table)?;
    let definitions = context.returning_columns(&insert.table)?;
    let request = context.returning_request(
        &insert.returning,
        &insert.returning_aliases,
        &insert.subqueries,
    )?;
    let retained = context.statements.read_control()?;
    let control = ProductionControl::new(
        retained.memory(),
        retained.cancellation(),
        context.cancellation,
    );
    let mut outcome = RuleExecutionOutcome::default();
    for rule in rules.iter() {
        let rule = rule?;
        for action in &rule.definition.actions {
            let target_columns = rule_action_target_columns_with_control(
                context.analysis.sources,
                action,
                &control,
            )?;
            let relation = InternalRelationId::allocate();
            let mut resolver = OwnedRuleQueryResolver::from_insert_with_control(
                context.analysis,
                insert,
                relation,
                &control,
            )?;
            let mut bound = BoundRuleStatement::bind(
                context.analysis.sources,
                action,
                &*target_columns,
                &mut resolver,
                &control,
            )?;
            let condition = resolver.condition_with_control(
                context.binding,
                rule,
                &definitions,
                &privilege_subject,
                &control,
            )?;
            let consumer = returning_consumer(
                context,
                &definitions,
                action,
                &mut bound,
                request,
                &*target_columns,
                outcome.returning.is_some(),
            )?;
            let input = if matches!(action, Statement::Notify { .. }) {
                None
            } else {
                let required = resolver.input_columns_with_control(
                    context.analysis,
                    insert,
                    action,
                    rule.definition.condition.as_ref(),
                    &*target_columns,
                    &control,
                )?;
                let query =
                    copy_rule_input_query(context, insert, scope, required, prune, &control)?;
                Some(OwnedRuleQueryInput {
                    query,
                    inline: insert
                        .rule_values_input
                        .as_ref()
                        .is_some_and(uqa_sql::plan::RuleValuesInput::is_inline),
                    relation,
                    condition,
                    expressions: control
                        .finish(resolver.expressions, control.empty_reservation())?,
                })
            };
            let result = context.execute_bound_action(
                &bound,
                input,
                params,
                scope,
                &privilege_subject,
                consumer
                    .as_ref()
                    .map(|consumer| Rc::clone(consumer) as Rc<dyn QueryRowConsumer>),
            )?;

            complete_action(
                &mut outcome,
                RuleEvent::Insert,
                rule,
                action,
                result,
                consumer,
            )?;
        }
    }
    Ok(outcome)
}

fn copy_rule_input_query<S: Clone + 'static>(
    context: &RuleContext<'_, S>,
    insert: &InsertPlan,
    scope: &crate::query::CteScope<S>,
    required: Option<RuleInputColumns>,
    prune: crate::mutation::views::commands::SourceOutputPruning,
    control: &ProductionControl<'_>,
) -> Result<uqa_sql::plan::MutableQueryPlan, SQLError> {
    let mut query = insert
        .source
        .as_deref()
        .expect("query input")
        .copy_for_rewrite(control)?;
    if let Some(required) = required {
        query = (prune.owned)(
            context.volatility,
            context.returning.routines,
            query,
            &|position| required.positions.binary_search(&position).is_ok(),
            required.width,
            &|name| insert.ctes.iter().any(|cte| cte.name == name) || scope.contains_cte_name(name),
            control,
        )?;
    }
    Ok(query)
}

pub(super) fn complete_action(
    outcome: &mut RuleExecutionOutcome,
    event: RuleEvent,
    rule: &uqa_sql::catalog::events::StoredRule,
    action: &Statement,
    result: StatementOutput,
    consumer: Option<Rc<super::returning::RuleReturningConsumer<'_>>>,
) -> Result<(), SQLError> {
    if rule.definition.instead
        && rule.definition.condition.is_none()
        && matches!(
            (event, action),
            (RuleEvent::Insert, Statement::Insert(_))
                | (RuleEvent::Update, Statement::Update(_))
                | (RuleEvent::Delete, Statement::Delete(_))
        )
    {
        outcome.affected_rows = match &result {
            StatementOutput::Mutation(result) => result.affected_rows,
            StatementOutput::Command(result) => result.affected_rows,
            StatementOutput::Query(_) => 0,
        };
        outcome.sets_command_tag = true;
    }
    if let Some(consumer) = consumer {
        let StatementOutput::Mutation(result) = result else {
            return Err(SQLError::Internal(
                "rule RETURNING provider is not a mutation".into(),
            ));
        };
        let consumer = Rc::try_unwrap(consumer).map_err(|_| {
            SQLError::Internal("completed rule action retained its row consumer".into())
        })?;
        outcome.returning = Some(consumer.finish(result.affected_rows));
    }
    Ok(())
}

fn returning_consumer<'a, S: Clone + 'static>(
    context: &RuleContext<'a, S>,
    definitions: &'a [uqa_sql::ast::ColumnDef],
    action: &Statement,
    bound: &mut BoundRuleStatement,
    request: RuleReturningRequest,
    target_columns: &dyn ActionColumns,
    previous: bool,
) -> Result<Option<Rc<super::returning::RuleReturningConsumer<'a>>>, SQLError> {
    if !request.captures() || !statement_has_returning(action) {
        bound.clear_returning()?;
        return Ok(None);
    }
    if previous {
        return Err(SQLError::Routine {
            sqlstate: "0A000".into(),
            message: "cannot have RETURNING lists in multiple rules".into(),
        });
    }
    let provider = uqa_sql::semantics::rules::returning::rule_action_returning_schema(
        context.analysis.sources,
        context.returning,
        action,
        RuleEvent::Insert,
        definitions,
    )?;
    validate_rule_returning_provider_width(provider.len(), definitions.len())?;
    bound.augment_returning(None, definitions.len(), request, target_columns)?;
    Ok(Some(Rc::new(super::returning::RuleReturningConsumer::new(
        context.assignment,
        definitions,
        context.projection,
        ReturningRows::with_budget(
            context.memory.work_mem_bytes()?,
            context.cancellation.clone(),
        ),
    ))))
}

pub fn run_insert_query_rules<S: Clone + 'static>(
    rules: &RuleContext<'_, S>,
    context: crate::mutation::returning::ReturningExecutionContext<'_, S>,
    queries: &uqa_sql::semantics::rules::binding::OwnedInsertRuleQueries,
    params: &[SQLParam],
    scope: &crate::query::CteScope<S>,
    prune: crate::mutation::views::commands::SourceOutputPruning,
) -> Result<crate::mutation::output::MutationOutput, SQLError> {
    let mut output = None;
    let mut affected = 0;
    for insert in queries.relations().rev() {
        let projection = super::RuleOutputProjection::insert(context, insert, params, scope);
        let outcome = execute_insert_query_rules(
            &rules.with_projection(&projection),
            insert,
            params,
            scope,
            prune,
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
            output = Some(returning.project(context, projection.shape)?);
        }
    }
    Ok(output.unwrap_or_else(|| crate::mutation::output::MutationOutput::from_affected(affected)))
}
