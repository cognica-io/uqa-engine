//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `PostgreSQL` rewrite-rule qualification, OLD/NEW binding, action ordering, and recursion checks.

mod suppression;
pub(crate) use suppression::RuleSuppression;

mod columns;
pub use columns::{RuleColumnIter, RuleColumnSets};

mod indices;
pub(crate) use indices::RuleIndexBuffer;
pub(crate) use indices::RuleIndices;

#[cfg(test)]
mod column_tests;

mod returning;
mod row_source;
mod rows;
pub use rows::{RuleEventSource, RuleRowBuffer, RuleRows};

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};

use uqa_core::Value;
use uqa_sql::ast::{RuleEvent, Statement};
use uqa_sql::plpgsql::VariableResolver;
use uqa_sql::semantics::sets::placement_scope::SetPlacementScope;
use uqa_sql::SQLError;

use uqa_core::RelationIdentity;
mod context;
mod inputs;
pub use context::{RuleContext, RuleExpressions, RuleSecurity, RuleStatements};
pub use inputs::RuleInputProjection;

pub use crate::mutation::row_images::RuleRowImage;
use returning::capture_rule_returning_result;
pub use returning::RuleReturningResult;
use uqa_sql::semantics::rules::binding::bind_insert_values_action_from_rows_with_storage;
pub use uqa_sql::semantics::rules::returning::RuleReturningRequest;
pub use uqa_sql::semantics::rules::validate_rule_returning_contract;
use uqa_sql::semantics::rules::{
    analysis::{rule_columns, rule_condition_plan_references_row, rule_returning_columns},
    clear_statement_returning,
    returning::{augment_rule_returning_action, validate_rule_returning_provider_width},
    statement_has_returning,
};

thread_local! {
    static RULE_EXECUTION_STACK: RefCell<Vec<(String, RuleEvent)>> = const { RefCell::new(Vec::new()) };
}

struct PreparedRule {
    rule: uqa_sql::catalog::events::StoredRule,
    matching_rows: RuleIndices,
    condition_references_row: bool,
    action_references_row: Vec<bool>,
    action_row_columns: Vec<BTreeSet<String>>,
    action_columns: Vec<BTreeSet<String>>,
}

use uqa_sql::semantics::rules::binding::RuleColumnMetadata;

pub struct PreparedRuleBatch {
    table: String,
    event: RuleEvent,
    rows: RuleRows,
    rules: Vec<PreparedRule>,
    suppress_original: RuleSuppression,
    action_qualification_count: Option<usize>,
    work_mem: usize,
    cancellation: uqa_core::CancellationToken,
}

pub struct RuleExecutionOutcome {
    pub returning: Option<RuleReturningResult>,
    pub affected_rows: u64,
    pub sets_command_tag: bool,
}

#[derive(Clone, Copy)]
pub enum RuleRowSide {
    Old,
    New,
}

impl PreparedRuleBatch {
    pub fn event_row_count(&self) -> usize {
        self.rows.len()
    }

    pub fn set_action_qualification_count(&mut self, count: usize) {
        debug_assert!(matches!(self.event, RuleEvent::Update | RuleEvent::Delete));
        self.action_qualification_count = Some(count);
    }

    pub fn suppresses(&self, index: usize) -> Result<bool, SQLError> {
        self.suppress_original.suppresses(index)
    }

    pub fn supplement_rows(&mut self, rows: Vec<RuleRowImage>) -> Result<(), SQLError> {
        if rows.len() != self.rows.len() {
            return Err(SQLError::Internal(
                "rewrite-rule supplemental row count changed after qualification".into(),
            ));
        }
        self.supplement_rows_from_iter(rows.into_iter().map(Ok))
    }

    pub fn supplement_rows_from_iter(
        &mut self,
        rows: impl IntoIterator<Item = Result<RuleRowImage, SQLError>>,
    ) -> Result<(), SQLError> {
        let mut rows = rows.into_iter();
        let mut output = RuleRowBuffer::new(self.work_mem, self.cancellation.clone());
        for target in self.rows.iter() {
            let supplemental = rows.next().ok_or_else(|| {
                SQLError::Internal(
                    "rewrite-rule supplemental row count changed after qualification".into(),
                )
            })??;
            let mut target = target?;
            target.supplement_documents(supplemental);
            output.push(target)?;
        }
        if rows.next().transpose()?.is_some() {
            return Err(SQLError::Internal(
                "rewrite-rule supplemental row count changed after qualification".into(),
            ));
        }
        self.rows = output.finish();
        Ok(())
    }

    pub fn execute_actions(
        &self,
        context: RuleContext<'_>,
        request: RuleReturningRequest,
    ) -> Result<Option<RuleReturningResult>, SQLError> {
        Ok(self
            .execute_actions_with_affected(context, request)?
            .returning)
    }

    #[expect(
        clippy::too_many_lines,
        reason = "preserves action and RETURNING order"
    )]
    pub fn execute_actions_with_affected(
        &self,
        context: RuleContext<'_>,
        request: RuleReturningRequest,
    ) -> Result<RuleExecutionOutcome, SQLError> {
        if self.rules.is_empty() {
            return Ok(RuleExecutionOutcome {
                returning: None,
                affected_rows: 0,
                sets_command_tag: false,
            });
        }
        let privilege_subject = context.security.privilege_subject(&self.table)?;
        let _guard = RuleExecutionGuard::enter(&self.table, self.event)?;
        let columns = rule_columns(context.analysis, &self.table)?;
        let returning_columns = request
            .captures()
            .then(|| rule_returning_columns(context.analysis, &self.table))
            .transpose()?;
        let provider_exists = request.captures()
            && self.rules.iter().any(|prepared| {
                prepared
                    .rule
                    .definition
                    .actions
                    .iter()
                    .any(statement_has_returning)
            });
        let mut returning = provider_exists.then(RuleReturningResult::empty);
        let mut affected_rows = 0_u64;
        let mut sets_command_tag = false;
        let mut provider_captured = false;
        for prepared in &self.rules {
            for (action_index, action) in prepared.rule.definition.actions.iter().enumerate() {
                let action_columns =
                    prepared.action_columns.get(action_index).ok_or_else(|| {
                        SQLError::Internal(
                            "rewrite rule lost its prepared action column contract".into(),
                        )
                    })?;
                let reevaluated_rows;
                let reevaluated_matching_rows;
                let (action_matching_rows, action_rows) = if self.event == RuleEvent::Insert
                    && prepared.rule.bound_condition_plan().is_some()
                {
                    let mut output = RuleRowBuffer::new(self.work_mem, self.cancellation.clone());
                    let mut matched =
                        RuleIndexBuffer::new(self.work_mem, self.cancellation.clone())?;
                    for (row_index, row) in self.rows.iter().enumerate() {
                        let mut row = row?;
                        if rule_condition_matches(
                            context,
                            &prepared.rule,
                            &privilege_subject,
                            row_index,
                            &mut row,
                            &columns,
                            &mut |_, _, _| Ok(RuleInputProjection::default()),
                        )? {
                            matched.push(row_index)?;
                        }
                        output.push(row)?;
                    }
                    reevaluated_rows = output.finish();
                    reevaluated_matching_rows = matched.finish()?;
                    (&reevaluated_matching_rows, &reevaluated_rows)
                } else {
                    (&prepared.matching_rows, &self.rows)
                };
                let action_returns = statement_has_returning(action);
                let captures_action = request.captures() && action_returns;
                let mut captures_source_context = false;
                if captures_action && matches!(action, Statement::Update(_) | Statement::Delete(_))
                {
                    for index in action_matching_rows.reader()? {
                        if action_rows.get(index?)?.context.is_some() {
                            captures_source_context = true;
                            break;
                        }
                    }
                }
                let action_references_row = prepared
                    .action_references_row
                    .get(action_index)
                    .copied()
                    .unwrap_or(false);
                let uses_action_qualification =
                    matches!(self.event, RuleEvent::Update | RuleEvent::Delete)
                        && !prepared.condition_references_row
                        && !action_references_row
                        && !captures_source_context
                        && self.action_qualification_count.is_some();
                let qualification_rows;
                let qualification_indices;
                let (matching_rows, rows) = if uses_action_qualification {
                    let count = self.action_qualification_count.unwrap_or_default();
                    let mut matched = RuleRowBuffer::new(self.work_mem, self.cancellation.clone());
                    let mut matched_count = 0;
                    for qualification_index in 0..count {
                        let mut row = RuleRowImage::empty();
                        if rule_condition_matches(
                            context,
                            &prepared.rule,
                            &privilege_subject,
                            qualification_index,
                            &mut row,
                            &columns,
                            &mut |_, _, _| Ok(RuleInputProjection::default()),
                        )? {
                            matched.push(row)?;
                            matched_count += 1;
                        }
                    }
                    qualification_rows = matched.finish();
                    qualification_indices =
                        RuleIndices::all(matched_count, self.cancellation.clone());
                    (&qualification_indices, &qualification_rows)
                } else {
                    (action_matching_rows, action_rows)
                };
                let bind_action = |resolver: &mut dyn VariableResolver| {
                    uqa_sql::semantics::rules::action_binding::bind_rule_action(
                        context.analysis.sources,
                        action,
                        action_columns,
                        resolver,
                    )
                };
                let needs_row_source = !matches!(action, Statement::Notify { .. })
                    && (self.event == RuleEvent::Insert
                        || prepared.condition_references_row
                        || action_references_row
                        || captures_source_context
                        || uses_action_qualification);
                if needs_row_source
                    && matching_rows.len() > 1
                    && uqa_sql::semantics::rules::action_binding::rule_action_has_set_operation(
                        action,
                    )
                {
                    return Err(SQLError::Routine {
                        sqlstate: "0A000".into(),
                        message:
                            "conditional UNION/INTERSECT/EXCEPT statements are not implemented"
                                .into(),
                    });
                }
                let (mut bound, source_index, _input_scope) = if needs_row_source
                    && matches!(action, Statement::Insert(insert) if !insert.rows.is_empty())
                {
                    (
                        bind_insert_values_action_from_rows_with_storage(
                            matching_rows.reader()?.map(|index| rows.get(index?)),
                            &columns,
                            &bind_action,
                            Some(&crate::scalar::RetainedConstants::new(self.work_mem)),
                        )?,
                        None,
                        None,
                    )
                } else if needs_row_source {
                    let (bound, scope) = row_source::bind_retained_action_from_rows(
                        matching_rows.reader()?.map(|index| {
                            let index = index?;
                            rows.get(index).map(|row| (index, row))
                        }),
                        &columns,
                        context.memory.work_mem_bytes()?,
                        context.cancellation,
                        &bind_action,
                    )?;
                    let source_index = captures_source_context.then_some(bound.source_index);
                    (bound.statement, source_index, Some(scope))
                } else {
                    (action.clone(), None, None)
                };
                if captures_action {
                    if provider_captured {
                        return Err(SQLError::Routine {
                            sqlstate: "0A000".into(),
                            message: "cannot have RETURNING lists in multiple rules".into(),
                        });
                    }
                    provider_captured = true;
                    let event_width = returning_columns.as_ref().map_or(0, Vec::len);
                    let provider_width =
                        uqa_sql::semantics::returning::dml_statement_returning_schema(
                            context.returning,
                            bound.clone(),
                        )?
                        .ok_or_else(|| {
                            SQLError::Internal(
                                "rewrite-rule RETURNING provider lost its declared row type".into(),
                            )
                        })?
                        .len();
                    validate_rule_returning_provider_width(provider_width, event_width)?;
                    augment_rule_returning_action(
                        &mut bound,
                        source_index,
                        event_width,
                        request,
                        action_columns,
                    )?;
                } else if action_returns {
                    clear_statement_returning(&mut bound);
                }
                // A rule action runs as it was stored; its set-returning calls are not placed again.
                let result = {
                    let _stored = SetPlacementScope::stored_definition();
                    context.statements.execute(bound, &privilege_subject)
                }?;
                if prepared.rule.definition.instead
                    && prepared.rule.definition.condition.is_none()
                    && matches!(
                        (self.event, action),
                        (RuleEvent::Insert, Statement::Insert(_))
                            | (RuleEvent::Update, Statement::Update(_))
                            | (RuleEvent::Delete, Statement::Delete(_))
                    )
                {
                    affected_rows = match &result {
                        crate::statement::plan_executor::StatementOutput::Mutation(result) => {
                            result.affected_rows
                        }
                        crate::statement::plan_executor::StatementOutput::Command(result) => {
                            result.affected_rows
                        }
                        crate::statement::plan_executor::StatementOutput::Query(_) => 0,
                    };
                    sets_command_tag = true;
                }
                if captures_action {
                    let crate::statement::plan_executor::StatementOutput::Mutation(result) = result
                    else {
                        return Err(SQLError::Internal(
                            "rewrite-rule RETURNING provider is not a mutation".into(),
                        ));
                    };
                    let definitions = returning_columns.as_deref().ok_or_else(|| {
                        SQLError::Internal(
                            "rewrite-rule RETURNING capture lost the event row type".into(),
                        )
                    })?;
                    let captured = capture_rule_returning_result(
                        super::returning::ReturningRows::with_budget(
                            context.memory.work_mem_bytes()?,
                            context.cancellation.clone(),
                        ),
                        context.assignment,
                        result,
                        definitions,
                        captures_source_context.then(|| self.rows.clone()),
                    )?;
                    returning = Some(captured);
                }
            }
        }
        Ok(RuleExecutionOutcome {
            returning,
            affected_rows,
            sets_command_tag,
        })
    }
}

pub fn prepare_rule_batch(
    context: RuleContext<'_>,
    table: &str,
    event: RuleEvent,
    rows: Vec<RuleRowImage>,
) -> Result<PreparedRuleBatch, SQLError> {
    prepare_rule_batch_with_projection(context, table, event, rows, |_, _, _| {
        Ok(RuleInputProjection::default())
    })
}

pub fn prepare_rule_batch_with_projection<F>(
    context: RuleContext<'_>,
    table: &str,
    event: RuleEvent,
    rows: Vec<RuleRowImage>,
    project: F,
) -> Result<PreparedRuleBatch, SQLError>
where
    F: FnMut(usize, RuleRowSide, &BTreeSet<String>) -> Result<RuleInputProjection, SQLError>,
{
    prepare_rule_input(
        context,
        table,
        event,
        |bytes, cancellation| RuleRows::from_rows(rows.into_iter().map(Ok), bytes, cancellation),
        project,
    )
}

pub fn prepare_retained_rule_batch(
    context: RuleContext<'_>,
    table: &str,
    event: RuleEvent,
    rows: RuleRows,
) -> Result<PreparedRuleBatch, SQLError> {
    prepare_retained_rule_batch_with_projection(context, table, event, rows, |_, _, _| {
        Ok(RuleInputProjection::default())
    })
}

pub fn prepare_retained_rule_batch_with_projection<F>(
    context: RuleContext<'_>,
    table: &str,
    event: RuleEvent,
    rows: RuleRows,
    project: F,
) -> Result<PreparedRuleBatch, SQLError>
where
    F: FnMut(usize, RuleRowSide, &BTreeSet<String>) -> Result<RuleInputProjection, SQLError>,
{
    prepare_rule_input(context, table, event, |_, _| Ok(rows), project)
}

fn prepare_rule_input<F>(
    context: RuleContext<'_>,
    table: &str,
    event: RuleEvent,
    input: impl FnOnce(usize, uqa_core::CancellationToken) -> Result<RuleRows, SQLError>,
    mut project: F,
) -> Result<PreparedRuleBatch, SQLError>
where
    F: FnMut(usize, RuleRowSide, &BTreeSet<String>) -> Result<RuleInputProjection, SQLError>,
{
    let table = context
        .analysis
        .rules
        .resolve_rule_relation(table)?
        .qualified_name();
    let rules = context.analysis.rules.rules_for(&table, event)?;
    if !rules.is_empty() {
        ensure_not_recursive(&table, event)?;
    }
    let work_mem = context.memory.work_mem_bytes()?;
    let cancellation = context.cancellation.clone();
    let mut rows = input(work_mem, cancellation.clone())?;
    if rules.is_empty() {
        return Ok(PreparedRuleBatch {
            table,
            event,
            suppress_original: RuleSuppression::new(rows.len(), cancellation.clone()),
            rows,
            rules: Vec::new(),
            action_qualification_count: None,
            work_mem,
            cancellation,
        });
    }
    let columns = rule_columns(context.analysis, &table)?;
    let privilege_subject = context.security.privilege_subject(&table)?;
    let mut suppress_original = RuleSuppression::new(rows.len(), cancellation.clone());
    let mut prepared_rules = Vec::with_capacity(rules.len());
    for rule in rules {
        let mut prepared = prepare_rule_actions(context, rule, &columns)?;
        if prepared.rule.definition.condition.is_none() {
            prepared.matching_rows = RuleIndices::all(rows.len(), cancellation.clone());
            if prepared.rule.definition.instead {
                suppress_original.suppress_all()?;
            }
            prepared_rules.push(prepared);
            continue;
        }
        let mut projected = RuleRowBuffer::new(work_mem, cancellation.clone());
        let mut matching_rows = RuleIndexBuffer::new(work_mem, cancellation.clone())?;
        for (index, row) in rows.iter().enumerate() {
            let mut row = row?;
            if rule_condition_matches(
                context,
                &prepared.rule,
                &privilege_subject,
                index,
                &mut row,
                &columns,
                &mut project,
            )? {
                matching_rows.push(index)?;
                if prepared.rule.definition.instead {
                    suppress_original.suppress(index)?;
                }
            }
            projected.push(row)?;
        }
        rows = projected.finish();
        prepared.matching_rows = matching_rows.finish()?;
        prepared_rules.push(prepared);
    }
    Ok(PreparedRuleBatch {
        table,
        event,
        rows,
        rules: prepared_rules,
        suppress_original,
        action_qualification_count: None,
        work_mem,
        cancellation,
    })
}

struct RuleExecutionGuard {
    table: String,
    event: RuleEvent,
}

impl RuleExecutionGuard {
    fn enter(table: &str, event: RuleEvent) -> Result<Self, SQLError> {
        ensure_not_recursive(table, event)?;
        RULE_EXECUTION_STACK.with(|stack| stack.borrow_mut().push((table.to_string(), event)));
        Ok(Self {
            table: table.to_string(),
            event,
        })
    }
}

impl Drop for RuleExecutionGuard {
    fn drop(&mut self) {
        RULE_EXECUTION_STACK.with(|stack| {
            let popped = stack.borrow_mut().pop();
            debug_assert_eq!(popped, Some((self.table.clone(), self.event)));
        });
    }
}

fn ensure_not_recursive(table: &str, event: RuleEvent) -> Result<(), SQLError> {
    let recursive = RULE_EXECUTION_STACK.with(|stack| {
        stack
            .borrow()
            .iter()
            .any(|(active_table, active_event)| active_table == table && *active_event == event)
    });
    if !recursive {
        return Ok(());
    }
    let relation = RelationIdentity::from_legacy_name(table)
        .map_err(|error| SQLError::Internal(format!("decode rule relation `{table}`: {error}")))?;
    Err(SQLError::Routine {
        sqlstate: "42P17".into(),
        message: format!(
            "infinite recursion detected in rules for relation \"{}\"",
            relation.name
        ),
    })
}

mod condition_binding;
use condition_binding::rule_condition_matches;

pub mod views;

fn prepare_rule_actions(
    context: RuleContext<'_>,
    mut rule: uqa_sql::catalog::events::StoredRule,
    columns: &BTreeMap<String, RuleColumnMetadata>,
) -> Result<PreparedRule, SQLError> {
    if let Some((plan, binding)) = rule
        .condition_plan
        .as_mut()
        .zip(rule.condition_binding.as_ref())
    {
        let mut ordered = columns.iter().collect::<Vec<_>>();
        ordered.sort_by_key(|(_, column)| column.position);
        let binding = binding.refresh_columns(
            ordered.iter().map(|(name, _)| (*name).clone()).collect(),
            plan,
        );
        if uqa_sql::semantics::rules::action_binding::rule_condition_plan_references_whole_row(plan)
        {
            let columns = ordered
                .into_iter()
                .map(|(name, column)| {
                    let mut definition =
                        uqa_sql::ast::ColumnDef::nullable(name.clone(), column.ty.clone());
                    definition.collation_oid = column.collation_oid;
                    definition
                })
                .collect::<Vec<_>>();
            context
                .binding
                .bind_rule_inputs(plan, &binding, &rule.definition.table, &columns)?;
        }
        rule.condition_binding = Some(binding);
    }
    if let Some(condition) = &mut rule.condition_plan {
        context.expressions.prepare_condition(condition)?;
    }
    let condition_references_row = if rule.bound_condition_plan().is_some() {
        rule_condition_plan_references_row(&rule)
    } else {
        rule.definition
            .condition
            .as_ref()
            .is_some_and(uqa_sql::semantics::rules::action_binding::rule_expr_references_row)
    };
    let action_columns = rule
        .definition
        .actions
        .iter()
        .map(|action| {
            uqa_sql::semantics::rules::action_binding::rule_action_target_columns(
                context.analysis.sources,
                action,
            )
        })
        .collect::<Result<Vec<_>, SQLError>>()?;
    let action_references_row = rule
        .definition
        .actions
        .iter()
        .zip(&action_columns)
        .map(|(action, columns)| {
            uqa_sql::semantics::rules::action_binding::rule_statement_references_row(
                context.analysis.sources,
                action,
                columns,
            )
        })
        .collect::<Result<Vec<_>, SQLError>>()?;
    let action_row_columns = rule
        .definition
        .actions
        .iter()
        .zip(&action_columns)
        .map(|(action, action_columns)| {
            if uqa_sql::semantics::rules::action_binding::rule_statement_references_whole_row(
                context.analysis.sources,
                action,
                action_columns,
            )? {
                Ok(columns.keys().cloned().collect())
            } else {
                uqa_sql::semantics::rules::action_binding::rule_statement_row_columns(
                    context.analysis.sources,
                    action,
                    action_columns,
                )
            }
        })
        .collect::<Result<Vec<_>, SQLError>>()?;
    Ok(PreparedRule {
        rule,
        matching_rows: RuleIndices::all(0, context.cancellation.clone()),
        condition_references_row,
        action_references_row,
        action_row_columns,
        action_columns,
    })
}
