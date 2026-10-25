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
use std::{collections::BTreeSet, rc::Rc};
use uqa_sql::{
    ast::{InternalRelationId, RuleEvent, Statement},
    plan::{InsertPlan, QueryPlan},
    semantics::rules::{
        action_binding::{bind_rule_action, bind_rule_expr_scoped, rule_action_target_columns},
        binding::{InsertQueryResolver, RuleAction, RuleQueryInput},
        clear_statement_returning,
        returning::{augment_rule_returning_action, validate_rule_returning_provider_width},
        statement_has_returning,
    },
    SQLError, SQLParam,
};

fn execute_insert_query_rules<S: Clone + 'static>(
    context: &RuleContext<'_, S>,
    insert: &InsertPlan,
    params: &[SQLParam],
    scope: &crate::query::CteScope<S>,
    prune: fn(
        &dyn uqa_sql::semantics::volatility::VolatilityCatalog,
        &mut QueryPlan,
        &BTreeSet<usize>,
        usize,
    ),
) -> Result<RuleExecutionOutcome, SQLError> {
    let rules = context
        .analysis
        .rules
        .rules_for(&insert.table, RuleEvent::Insert)?;
    let _guard = RuleExecutionGuard::enter(&insert.table, RuleEvent::Insert)?;
    let privilege_subject = context.security.privilege_subject(&insert.table)?;
    let definitions = super::rule_returning_columns(context.analysis, &insert.table)?;
    let request = RuleReturningRequest::from_plan(
        &insert.returning,
        &insert.returning_aliases,
        &insert.subqueries,
    );
    let mut outcome = RuleExecutionOutcome {
        returning: None,
        affected_rows: 0,
        sets_command_tag: false,
    };
    for rule in rules {
        for action in &rule.definition.actions {
            let target_columns = rule_action_target_columns(context.analysis.sources, action)?;
            let relation = InternalRelationId::allocate();
            let mut resolver = InsertQueryResolver::new(context.analysis, insert, relation)?;
            let mut bound = bind_rule_action(
                context.analysis.sources,
                action,
                &target_columns,
                &mut resolver,
            )?;
            let condition = rule
                .definition
                .condition
                .as_ref()
                .map(|condition| bind_rule_expr_scoped(condition, &mut resolver, &BTreeSet::new()))
                .transpose()?;
            let captures = request.captures() && statement_has_returning(action);
            let consumer = if captures {
                if outcome.returning.is_some() {
                    return Err(SQLError::Routine {
                        sqlstate: "0A000".into(),
                        message: "cannot have RETURNING lists in multiple rules".into(),
                    });
                }
                Some(returning_consumer(
                    context,
                    &definitions,
                    action,
                    &mut bound,
                    request,
                    &target_columns,
                )?)
            } else {
                clear_statement_returning(&mut bound);
                None
            };
            let input = if matches!(action, Statement::Notify { .. }) {
                None
            } else {
                let mut query = insert.source.as_deref().expect("query input").clone();
                if let Some((positions, width)) = resolver.input_columns(
                    context.analysis,
                    insert,
                    action,
                    rule.definition.condition.as_ref(),
                    &target_columns,
                )? {
                    prune(context.volatility, &mut query, &positions, width);
                }
                Some(RuleQueryInput {
                    query,
                    relation,
                    condition,
                    expressions: resolver.expressions,
                })
            };
            let result = {
                let _stored =
                    uqa_sql::semantics::sets::placement_scope::SetPlacementScope::stored_definition(
                    );
                context.statements.execute(
                    RuleAction {
                        statement: bound,
                        input,
                    },
                    params,
                    Some(scope),
                    &privilege_subject,
                    consumer
                        .as_ref()
                        .map(|consumer| Rc::clone(consumer) as Rc<dyn QueryRowConsumer>),
                )?
            };
            complete_action(&mut outcome, &rule, action, result, consumer)?;
        }
    }
    Ok(outcome)
}

fn complete_action(
    outcome: &mut RuleExecutionOutcome,
    rule: &uqa_sql::catalog::events::StoredRule,
    action: &Statement,
    result: StatementOutput,
    consumer: Option<Rc<super::returning::RuleReturningConsumer<'_>>>,
) -> Result<(), SQLError> {
    if rule.definition.instead
        && rule.definition.condition.is_none()
        && matches!(action, Statement::Insert(_))
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
    bound: &mut Statement,
    request: RuleReturningRequest,
    target_columns: &BTreeSet<String>,
) -> Result<Rc<super::returning::RuleReturningConsumer<'a>>, SQLError> {
    let provider = uqa_sql::semantics::returning::dml_statement_returning_schema(
        context.returning,
        action.clone(),
    )?
    .ok_or_else(|| SQLError::Internal("rule action lost its RETURNING descriptor".into()))?;
    validate_rule_returning_provider_width(provider.len(), definitions.len())?;
    augment_rule_returning_action(bound, None, definitions.len(), request, target_columns)?;
    Ok(Rc::new(super::returning::RuleReturningConsumer::new(
        context.assignment,
        definitions,
        None,
        context.projection,
        ReturningRows::with_budget(
            context.memory.work_mem_bytes()?,
            context.cancellation.clone(),
        ),
    )))
}

pub fn run_insert_query_rules<S: Clone + 'static>(
    rules: &RuleContext<'_, S>,
    context: crate::mutation::returning::ReturningExecutionContext<'_, S>,
    queries: &uqa_sql::semantics::rules::binding::InsertRuleQueries,
    params: &[SQLParam],
    scope: &crate::query::CteScope<S>,
    prune: fn(
        &dyn uqa_sql::semantics::volatility::VolatilityCatalog,
        &mut QueryPlan,
        &BTreeSet<usize>,
        usize,
    ),
) -> Result<crate::mutation::output::MutationOutput, SQLError> {
    let mut output = None;
    let mut affected = 0;
    for insert in queries.relations.iter().rev() {
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
