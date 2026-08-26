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
use uqa_sql::plpgsql::{bind_expr, bind_statement, ResolvedVariable, VariableResolver};
use uqa_sql::SQLError;
use uqa_storage::document_store::Document;

use crate::{Engine, RelationIdentity};

use actions::{bind_insert_values_action, bind_set_oriented_action};
use returning::{
    augment_rule_returning_action, capture_rule_returning_result, rule_returning_columns,
    statement_has_returning,
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
}

struct RuleColumnMetadata {
    declared_type: String,
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
        capture_returning: bool,
    ) -> Result<Option<RuleReturningResult>, SQLError> {
        if self.rules.is_empty() {
            return Ok(None);
        }
        let _guard = RuleExecutionGuard::enter(&self.table, self.event)?;
        let columns = rule_columns(engine, &self.table)?;
        let returning_columns = capture_returning
            .then(|| rule_returning_columns(engine, &self.table))
            .transpose()?;
        let provider_exists = capture_returning
            && self.rules.iter().any(|prepared| {
                prepared
                    .rule
                    .definition
                    .actions
                    .iter()
                    .any(statement_has_returning)
            });
        let mut returning = provider_exists.then(RuleReturningResult::empty);
        for (rule_index, prepared) in self.rules.iter().enumerate() {
            for (action_index, action) in prepared.rule.definition.actions.iter().enumerate() {
                if prepared.matching_rows.is_empty() {
                    continue;
                }
                let captures_action = capture_returning && statement_has_returning(action);
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
                let (mut bound, source_index) = if needs_row_source
                    && matches!(action, Statement::Insert(insert) if !insert.rows.is_empty())
                {
                    (
                        bind_insert_values_action(
                            engine,
                            self.event,
                            action,
                            &prepared.matching_rows,
                            &self.rows,
                            &columns,
                        )?,
                        None,
                    )
                } else if needs_row_source {
                    let bound = bind_set_oriented_action(
                        engine,
                        self.event,
                        action,
                        &prepared.matching_rows,
                        &self.rows,
                        &columns,
                        &format!("__uqa_rule_rows_{rule_index}_{action_index}"),
                    )?;
                    let source_index = captures_source_context.then_some(bound.source_index);
                    (bound.statement, source_index)
                } else {
                    (action.clone(), None)
                };
                if captures_action {
                    augment_rule_returning_action(engine, &mut bound, source_index)?;
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
                    if returning
                        .as_ref()
                        .is_some_and(RuleReturningResult::has_rows)
                    {
                        return Err(SQLError::Routine {
                            sqlstate: "0A000".into(),
                            message: "cannot have RETURNING lists in multiple rules".into(),
                        });
                    }
                    returning = Some(captured);
                }
            }
        }
        Ok(returning)
    }
}

fn rule_action_target_columns(
    engine: &Engine,
    action: &Statement,
) -> Result<BTreeSet<String>, SQLError> {
    let table = match action {
        Statement::Insert(statement) => &statement.table,
        Statement::Update(statement) => &statement.table,
        Statement::Delete(statement) => &statement.table,
        _ => return Ok(BTreeSet::new()),
    };
    Ok(engine
        .try_describe_table_row_type(table)
        .map_err(|error| SQLError::Internal(format!("read rule action row type: {error}")))?
        .ok_or_else(|| SQLError::UnknownTable(table.clone()))?
        .into_iter()
        .map(|column| column.name)
        .collect())
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
            .is_some_and(expr_references_rule_row);
        let action_references_row = rule
            .definition
            .actions
            .iter()
            .map(statement_references_rule_row)
            .collect::<Vec<_>>();
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
                    declared_type: column.ty.sql_name(),
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
            declared_type: Some(metadata.declared_type.clone()),
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

fn expr_references_rule_row(expr: &Expr) -> bool {
    expr.any_node(&|node| {
        matches!(
            node,
            Expr::QualifiedColumn { qualifier, .. }
                if qualifier.eq_ignore_ascii_case("old") || qualifier.eq_ignore_ascii_case("new")
        )
    })
}

struct RuleReferenceResolver {
    referenced: bool,
}

impl VariableResolver for RuleReferenceResolver {
    fn resolve_name(&mut self, _name: &str) -> Result<Option<ResolvedVariable>, SQLError> {
        Ok(None)
    }

    fn resolve_qualified(
        &mut self,
        qualifier: &str,
        _column: &str,
    ) -> Result<Option<ResolvedVariable>, SQLError> {
        if qualifier.eq_ignore_ascii_case("old") || qualifier.eq_ignore_ascii_case("new") {
            self.referenced = true;
        }
        Ok(None)
    }

    fn resolve_param(&mut self, _index: usize) -> Result<Option<ResolvedVariable>, SQLError> {
        Ok(None)
    }
}

fn statement_references_rule_row(statement: &Statement) -> bool {
    let mut resolver = RuleReferenceResolver { referenced: false };
    let _ = bind_statement(statement, &mut resolver);
    resolver.referenced
}
