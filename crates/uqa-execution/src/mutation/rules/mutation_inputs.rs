//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Run UPDATE/DELETE rewrite actions as independent queries before their original mutation.

use super::{RuleContext, RuleExecutionGuard, RuleExecutionOutcome, RuleReturningRequest};
use crate::{
    mutation::returning::ReturningRows,
    query::{consumer::QueryRowConsumer, CteScope},
};
use std::rc::Rc;
use uqa_core::memory::ProductionControl;
use uqa_sql::{
    ast::Statement,
    binding::correlation::CorrelationContext,
    semantics::rules::{
        action_binding::{
            rule_action_references_row_with_control, rule_action_target_columns_with_control,
            ActionColumns, BoundRuleStatement,
        },
        binding::{OwnedMutationRuleInput, RuleMutation},
        returning::validate_rule_returning_provider_width,
        statement_has_returning,
    },
    SQLError, SQLParam,
};

pub fn execute_mutation_query_rules<S: Clone + 'static>(
    context: &RuleContext<'_, S>,
    statement: RuleMutation<'_>,
    params: &[SQLParam],
    scope: &CteScope<S>,
) -> Result<RuleExecutionOutcome, SQLError> {
    let rules = context.capture_rules(statement.table(), statement.event())?;
    let _guard = RuleExecutionGuard::enter(statement.table(), statement.event())?;
    let rule_subject = context.security.privilege_subject(statement.table())?;
    let input_subject = scope.privilege_subject()?;
    let definitions = context.returning_columns(statement.table())?;
    let request = context.returning_request(
        statement.returning(),
        statement.returning_aliases(),
        statement.subqueries(),
    )?;
    let retained = context.statements.read_control()?;
    let control = ProductionControl::new(
        retained.memory(),
        retained.cancellation(),
        context.cancellation,
    );
    let binding = crate::query::binding::binding_context_with_control(scope, &control)?;
    let (target_schema, source_schema) =
        context.mutation_input_schemas(statement, params, &binding, &control)?;
    let mut outcome = RuleExecutionOutcome::default();
    for rule in rules.iter() {
        let rule = rule?;
        for action in &rule.definition.actions {
            let target_columns = rule_action_target_columns_with_control(
                context.analysis.sources,
                action,
                &control,
            )?;
            let references_event = rule_action_references_row_with_control(
                context.analysis.sources,
                action,
                &*target_columns,
                rule.definition.condition.as_ref(),
                &control,
            )?;
            let mut input = OwnedMutationRuleInput::bind(
                context.analysis,
                CorrelationContext {
                    routines: context.returning.routines,
                    catalog: binding.catalog.as_ref(),
                    resolution: &binding.resolution,
                    scoped_relations: Some(scope),
                },
                statement,
                (&target_schema, source_schema.as_ref()),
                references_event,
                input_subject,
                &rule_subject,
                &control,
            )?;
            let mut bound = BoundRuleStatement::bind(
                context.analysis.sources,
                action,
                &*target_columns,
                &mut input.resolver,
                &control,
            )?;
            let condition = input.resolver.condition_with_control(
                context.binding,
                rule,
                &definitions,
                &rule_subject,
                &control,
            )?;
            let consumer = returning_consumer(
                context,
                &definitions,
                action,
                statement.event(),
                &mut bound,
                request,
                &*target_columns,
                &input,
                outcome.returning.is_some(),
            )?;
            let input = (!matches!(action, Statement::Notify { .. }))
                .then(|| input.finish(condition, &control))
                .transpose()?;
            let result = context.execute_bound_action(
                &bound,
                input,
                params,
                scope,
                &rule_subject,
                consumer
                    .as_ref()
                    .map(|consumer| Rc::clone(consumer) as Rc<dyn QueryRowConsumer>),
            )?;

            super::query_inputs::complete_action(
                &mut outcome,
                statement.event(),
                rule,
                action,
                result,
                consumer,
            )?;
        }
    }
    Ok(outcome)
}

#[expect(
    clippy::too_many_arguments,
    reason = "keeps provider syntax, original event descriptors, and source context distinct"
)]
fn returning_consumer<'a, S: Clone + 'static>(
    context: &RuleContext<'a, S>,
    definitions: &'a [uqa_sql::ast::ColumnDef],
    action: &Statement,
    event: uqa_sql::ast::RuleEvent,
    bound: &mut BoundRuleStatement,
    request: RuleReturningRequest,
    target_columns: &dyn ActionColumns,
    input: &OwnedMutationRuleInput,
    has_returning: bool,
) -> Result<Option<Rc<super::returning::RuleReturningConsumer<'a>>>, SQLError> {
    if !request.captures() || !statement_has_returning(action) {
        bound.clear_returning()?;
        return Ok(None);
    }
    if has_returning {
        return Err(SQLError::Routine {
            sqlstate: "0A000".into(),
            message: "cannot have RETURNING lists in multiple rules".into(),
        });
    }
    let provider = uqa_sql::semantics::rules::returning::rule_action_returning_schema(
        context.analysis.sources,
        context.returning,
        action,
        event,
        definitions,
    )?;
    validate_rule_returning_provider_width(provider.len(), definitions.len())?;
    bound.augment_returning(None, definitions.len(), request, target_columns)?;
    let context_schema = if matches!(action, Statement::Update(_) | Statement::Delete(_)) {
        bound.append_returning_context(&input.source_context)?;
        input.source_context_schema.clone()
    } else {
        None
    };
    Ok(Some(Rc::new(
        super::returning::RuleReturningConsumer::new(
            context.assignment,
            definitions,
            context.projection,
            ReturningRows::with_budget(
                context.memory.work_mem_bytes()?,
                context.cancellation.clone(),
            ),
        )
        .with_context(context_schema),
    )))
}
