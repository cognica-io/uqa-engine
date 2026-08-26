//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `PostgreSQL` rewrite-rule qualification, OLD/NEW binding, action ordering, and recursion checks.

mod actions;
mod returning;

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};

use uqa_core::{DocId, Value};
use uqa_execution::OwnedPhysicalRow;
use uqa_sql::ast::{Expr, RuleEvent, Statement};
use uqa_sql::plpgsql::{bind_expr, ResolvedVariable, VariableResolver};
use uqa_sql::SQLError;
use uqa_storage::document_store::Document;

use crate::{Engine, RelationIdentity};

use actions::{bind_insert_values_action, bind_set_oriented_action};
pub(in crate::sql) use returning::RuleReturningRequest;
use returning::{
    augment_rule_returning_action, capture_rule_returning_result, clear_statement_returning,
    rule_returning_columns, statement_has_returning,
};
pub(in crate::sql) use returning::{validate_rule_returning_contract, RuleReturningResult};

use super::scalar::eval_lowered_expression;

thread_local! {
    static RULE_EXECUTION_STACK: RefCell<Vec<(String, RuleEvent)>> = const { RefCell::new(Vec::new()) };
}

#[derive(Clone)]
pub(in crate::sql) struct RuleRowImage {
    pub(in crate::sql) old_doc_id: Option<DocId>,
    pub(in crate::sql) old: Option<Document>,
    pub(in crate::sql) new_doc_id: Option<DocId>,
    pub(in crate::sql) new: Option<Document>,
    pub(in crate::sql) context: Option<OwnedPhysicalRow>,
}

struct PreparedRule {
    rule: crate::engine_events::StoredRule,
    matching_rows: Vec<usize>,
    condition_references_row: bool,
    action_references_row: Vec<bool>,
    action_columns: Vec<BTreeSet<String>>,
}

struct RuleColumnMetadata {
    ty: uqa_sql::ast::ColumnType,
    uses_document_id: bool,
}

pub(in crate::sql) struct PreparedRuleBatch {
    table: String,
    event: RuleEvent,
    rows: Vec<RuleRowImage>,
    rules: Vec<PreparedRule>,
    suppress_original: Vec<bool>,
}

impl PreparedRuleBatch {
    pub(in crate::sql) fn suppresses(&self, index: usize) -> bool {
        self.suppress_original.get(index).copied().unwrap_or(false)
    }

    pub(in crate::sql) fn execute_actions(
        &self,
        engine: &Engine,
        request: RuleReturningRequest,
    ) -> Result<Option<RuleReturningResult>, SQLError> {
        if self.rules.is_empty() {
            return Ok(None);
        }
        let _guard = RuleExecutionGuard::enter(&self.table, self.event)?;
        let columns = rule_columns(engine, &self.table)?;
        let returning_columns = request
            .captures()
            .then(|| rule_returning_columns(engine, &self.table))
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
        let mut provider_captured = false;
        for prepared in &self.rules {
            for (action_index, action) in prepared.rule.definition.actions.iter().enumerate() {
                let action_columns =
                    prepared.action_columns.get(action_index).ok_or_else(|| {
                        SQLError::Internal(
                            "rewrite rule lost its prepared action column contract".into(),
                        )
                    })?;
                let action_returns = statement_has_returning(action);
                let captures_action = request.captures() && action_returns;
                let captures_source_context = captures_action
                    && matches!(action, Statement::Update(_) | Statement::Delete(_))
                    && prepared.matching_rows.iter().any(|row_index| {
                        self.rows
                            .get(*row_index)
                            .is_some_and(|row| row.context.is_some())
                    });
                let needs_row_source = self.event == RuleEvent::Insert
                    || prepared.condition_references_row
                    || prepared
                        .action_references_row
                        .get(action_index)
                        .copied()
                        .unwrap_or(false)
                    || captures_source_context;
                if needs_row_source
                    && prepared.matching_rows.len() > 1
                    && crate::engine_events::rule_action_has_set_operation(action)
                {
                    return Err(SQLError::Routine {
                        sqlstate: "0A000".into(),
                        message:
                            "conditional UNION/INTERSECT/EXCEPT statements are not implemented"
                                .into(),
                    });
                }
                let (mut bound, source_index) = if needs_row_source
                    && matches!(action, Statement::Insert(insert) if !insert.rows.is_empty())
                {
                    (
                        bind_insert_values_action(
                            action,
                            &prepared.matching_rows,
                            &self.rows,
                            &columns,
                            action_columns,
                        )?,
                        None,
                    )
                } else if needs_row_source {
                    let bound = bind_set_oriented_action(
                        action,
                        &prepared.matching_rows,
                        &self.rows,
                        &columns,
                        action_columns,
                    )?;
                    let source_index = captures_source_context.then_some(bound.source_index);
                    (bound.statement, source_index)
                } else {
                    (action.clone(), None)
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
                let result = super::execute_compiled_statement(engine, bound, &[])?;
                if captures_action {
                    let definitions = returning_columns.as_deref().ok_or_else(|| {
                        SQLError::Internal(
                            "rewrite-rule RETURNING capture lost the event row type".into(),
                        )
                    })?;
                    let captured = capture_rule_returning_result(
                        result,
                        definitions,
                        captures_source_context.then_some(self.rows.as_slice()),
                    )?;
                    returning = Some(captured);
                }
            }
        }
        Ok(returning)
    }
}

pub(in crate::sql) fn prepare_rule_batch(
    engine: &Engine,
    table: &str,
    event: RuleEvent,
    rows: Vec<RuleRowImage>,
) -> Result<PreparedRuleBatch, SQLError> {
    let table = engine.resolve_rule_relation(table)?.qualified_name();
    let rules = engine.rules_for(&table, event)?;
    if rules.is_empty() {
        return Ok(PreparedRuleBatch {
            table,
            event,
            suppress_original: vec![false; rows.len()],
            rows,
            rules: Vec::new(),
        });
    }
    ensure_not_recursive(&table, event)?;
    let columns = rule_columns(engine, &table)?;
    let mut suppress_original = vec![false; rows.len()];
    let mut prepared_rules = Vec::with_capacity(rules.len());
    for rule in rules {
        let condition_references_row = rule
            .definition
            .condition
            .as_ref()
            .is_some_and(crate::engine_events::rule_expr_references_row);
        let action_columns = rule
            .definition
            .actions
            .iter()
            .map(|action| engine.rule_action_target_columns(action))
            .collect::<Result<Vec<_>, SQLError>>()?;
        let action_references_row = rule
            .definition
            .actions
            .iter()
            .zip(&action_columns)
            .map(|(action, columns)| {
                crate::engine_events::rule_statement_references_row(action, columns)
            })
            .collect::<Result<Vec<_>, SQLError>>()?;
        let mut matching_rows = Vec::new();
        for (index, row) in rows.iter().enumerate() {
            if rule_condition_matches(engine, rule.definition.condition.as_ref(), row, &columns)? {
                matching_rows.push(index);
                if rule.definition.instead {
                    suppress_original[index] = true;
                }
            }
        }
        prepared_rules.push(PreparedRule {
            rule,
            matching_rows,
            condition_references_row,
            action_references_row,
            action_columns,
        });
    }
    Ok(PreparedRuleBatch {
        table,
        event,
        rows,
        rules: prepared_rules,
        suppress_original,
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

fn rule_columns(
    engine: &Engine,
    table: &str,
) -> Result<BTreeMap<String, RuleColumnMetadata>, SQLError> {
    let columns = engine
        .try_describe_table_row_type(table)
        .map_err(|error| SQLError::Internal(format!("read rule row type: {error}")))?
        .ok_or_else(|| SQLError::UnknownTable(table.to_string()))?;
    Ok(columns
        .into_iter()
        .map(|column| {
            let uses_document_id = column.primary_key && column.ty.is_integer();
            (
                column.name,
                RuleColumnMetadata {
                    ty: column.ty,
                    uses_document_id,
                },
            )
        })
        .collect())
}

struct RuntimeRuleResolver<'a> {
    old: Option<&'a Document>,
    new: Option<&'a Document>,
    old_doc_id: Option<DocId>,
    new_doc_id: Option<DocId>,
    columns: &'a BTreeMap<String, RuleColumnMetadata>,
}

impl RuntimeRuleResolver<'_> {
    fn record_field(
        &self,
        record: Option<&Document>,
        doc_id: Option<DocId>,
        column: &str,
    ) -> Result<ResolvedVariable, SQLError> {
        let metadata = self
            .columns
            .get(column)
            .ok_or_else(|| SQLError::UnknownColumn(column.to_string()))?;
        let value = if let Some(value) = record.and_then(|record| record.get(column).cloned()) {
            value
        } else if metadata.uses_document_id {
            doc_id
                .map(i64::try_from)
                .transpose()
                .map_err(|_| {
                    SQLError::TypeMismatch("document id exceeds PostgreSQL bigint".into())
                })?
                .map_or(Value::Null, Value::Int)
        } else {
            Value::Null
        };
        Ok(ResolvedVariable {
            value,
            declared_type: Some(metadata.ty.sql_name()),
        })
    }
}

impl VariableResolver for RuntimeRuleResolver<'_> {
    fn resolve_name(&mut self, _name: &str) -> Result<Option<ResolvedVariable>, SQLError> {
        Ok(None)
    }

    fn resolve_qualified(
        &mut self,
        qualifier: &str,
        column: &str,
    ) -> Result<Option<ResolvedVariable>, SQLError> {
        if qualifier.eq_ignore_ascii_case("old") {
            return self
                .record_field(self.old, self.old_doc_id, column)
                .map(Some);
        }
        if qualifier.eq_ignore_ascii_case("new") {
            return self
                .record_field(self.new, self.new_doc_id, column)
                .map(Some);
        }
        Ok(None)
    }

    fn resolve_param(&mut self, _index: usize) -> Result<Option<ResolvedVariable>, SQLError> {
        Ok(None)
    }
}

fn rule_condition_matches(
    engine: &Engine,
    condition: Option<&Expr>,
    row: &RuleRowImage,
    columns: &BTreeMap<String, RuleColumnMetadata>,
) -> Result<bool, SQLError> {
    let Some(condition) = condition else {
        return Ok(true);
    };
    let condition = bind_expr(
        condition,
        &mut RuntimeRuleResolver {
            old: row.old.as_ref(),
            new: row.new.as_ref(),
            old_doc_id: row.old_doc_id,
            new_doc_id: row.new_doc_id,
            columns,
        },
    )?;
    Ok(uqa_sql::expr::truthy(&eval_lowered_expression(
        engine,
        &condition,
        None,
        &[],
    )?))
}
