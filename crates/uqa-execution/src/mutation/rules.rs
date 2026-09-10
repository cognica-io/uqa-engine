//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `PostgreSQL` rewrite-rule qualification, OLD/NEW binding, action ordering, and recursion checks.

mod returning;

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};

use uqa_core::Value;
use uqa_sql::ast::{BinaryOp, Expr, RuleEvent, Statement};
use uqa_sql::plpgsql::{bind_expr, ResolvedVariable, VariableResolver};
use uqa_sql::SQLError;
use uqa_storage::document_store::Document;

use uqa_core::RelationIdentity;
mod context;
pub use context::{RuleContext, RuleExpressions, RuleSecurity, RuleStatements};

pub use crate::mutation::row_images::RuleRowImage;
use returning::capture_rule_returning_result;
pub use returning::RuleReturningResult;
use uqa_sql::semantics::rules::binding::{bind_insert_values_action, bind_set_oriented_action};
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
    matching_rows: Vec<usize>,
    condition_references_row: bool,
    action_references_row: Vec<bool>,
    action_row_columns: Vec<BTreeSet<String>>,
    action_columns: Vec<BTreeSet<String>>,
}

use uqa_sql::semantics::rules::binding::RuleColumnMetadata;

pub struct PreparedRuleBatch {
    table: String,
    event: RuleEvent,
    rows: Vec<RuleRowImage>,
    rules: Vec<PreparedRule>,
    suppress_original: Vec<bool>,
    action_qualification_count: Option<usize>,
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

    pub fn suppresses(&self, index: usize) -> bool {
        self.suppress_original.get(index).copied().unwrap_or(false)
    }

    pub fn matched_action_row_columns(&self) -> Vec<BTreeSet<String>> {
        let mut rows = vec![BTreeSet::new(); self.rows.len()];
        for rule in &self.rules {
            for columns in &rule.action_row_columns {
                for row in &rule.matching_rows {
                    if let Some(required) = rows.get_mut(*row) {
                        required.extend(columns.iter().cloned());
                    }
                }
            }
        }
        rows
    }

    pub fn missing_action_row_columns(&self) -> Vec<(BTreeSet<String>, BTreeSet<String>)> {
        self.matched_action_row_columns()
            .into_iter()
            .zip(&self.rows)
            .map(|(columns, row)| {
                let missing = |record: &Option<Document>| {
                    if let Some(record) = record {
                        columns
                            .iter()
                            .filter(|column| !record.contains_key(*column))
                            .cloned()
                            .collect()
                    } else {
                        BTreeSet::new()
                    }
                };
                (missing(&row.old), missing(&row.new))
            })
            .collect()
    }

    pub fn supplement_rows(&mut self, rows: Vec<RuleRowImage>) -> Result<(), SQLError> {
        if rows.len() != self.rows.len() {
            return Err(SQLError::Internal(
                "rewrite-rule supplemental row count changed after qualification".into(),
            ));
        }
        for (target, supplemental) in self.rows.iter_mut().zip(rows) {
            target.supplement_documents(supplemental);
        }
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
                let mut reevaluated_rows;
                let reevaluated_matching_rows;
                let (action_matching_rows, action_rows) = if self.event == RuleEvent::Insert
                    && prepared.rule.bound_condition_plan().is_some()
                {
                    reevaluated_rows = self.rows.clone();
                    let mut matched = Vec::new();
                    for (row_index, row) in reevaluated_rows.iter_mut().enumerate() {
                        if rule_condition_matches(
                            context,
                            &prepared.rule,
                            &privilege_subject,
                            row_index,
                            row,
                            &columns,
                            &mut |_, _, _| Ok(None),
                        )? {
                            matched.push(row_index);
                        }
                    }
                    reevaluated_matching_rows = matched;
                    (
                        reevaluated_matching_rows.as_slice(),
                        reevaluated_rows.as_slice(),
                    )
                } else {
                    (prepared.matching_rows.as_slice(), self.rows.as_slice())
                };
                let action_returns = statement_has_returning(action);
                let captures_action = request.captures() && action_returns;
                let captures_source_context = captures_action
                    && matches!(action, Statement::Update(_) | Statement::Delete(_))
                    && action_matching_rows.iter().any(|row_index| {
                        action_rows
                            .get(*row_index)
                            .is_some_and(|row| row.context.is_some())
                    });
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
                    let mut matched = Vec::with_capacity(count);
                    for qualification_index in 0..count {
                        let mut row = RuleRowImage::empty();
                        if rule_condition_matches(
                            context,
                            &prepared.rule,
                            &privilege_subject,
                            qualification_index,
                            &mut row,
                            &columns,
                            &mut |_, _, _| Ok(None),
                        )? {
                            matched.push(row);
                        }
                    }
                    qualification_rows = matched;
                    qualification_indices = (0..qualification_rows.len()).collect::<Vec<_>>();
                    (
                        qualification_indices.as_slice(),
                        qualification_rows.as_slice(),
                    )
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
                let (mut bound, source_index) = if needs_row_source
                    && matches!(action, Statement::Insert(insert) if !insert.rows.is_empty())
                {
                    (
                        bind_insert_values_action(matching_rows, rows, &columns, &bind_action)?,
                        None,
                    )
                } else if needs_row_source {
                    let bound =
                        bind_set_oriented_action(matching_rows, rows, &columns, &bind_action)?;
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
                let result = context.statements.execute(bound, &privilege_subject)?;
                if prepared.rule.definition.instead
                    && prepared.rule.definition.condition.is_none()
                    && matches!(
                        (self.event, action),
                        (RuleEvent::Insert, Statement::Insert(_))
                            | (RuleEvent::Update, Statement::Update(_))
                            | (RuleEvent::Delete, Statement::Delete(_))
                    )
                {
                    affected_rows = result.affected_rows;
                    sets_command_tag = true;
                }
                if captures_action {
                    let definitions = returning_columns.as_deref().ok_or_else(|| {
                        SQLError::Internal(
                            "rewrite-rule RETURNING capture lost the event row type".into(),
                        )
                    })?;
                    let captured = capture_rule_returning_result(
                        context.assignment,
                        result,
                        definitions,
                        captures_source_context.then_some(self.rows.as_slice()),
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
    prepare_rule_batch_with_projection(context, table, event, rows, |_, _, _| Ok(None))
}

pub fn prepare_rule_batch_with_projection<F>(
    context: RuleContext<'_>,
    table: &str,
    event: RuleEvent,
    mut rows: Vec<RuleRowImage>,
    mut project: F,
) -> Result<PreparedRuleBatch, SQLError>
where
    F: FnMut(usize, RuleRowSide, &str) -> Result<Option<Value>, SQLError>,
{
    let table = context
        .analysis
        .rules
        .resolve_rule_relation(table)?
        .qualified_name();
    let rules = context.analysis.rules.rules_for(&table, event)?;
    if rules.is_empty() {
        return Ok(PreparedRuleBatch {
            table,
            event,
            suppress_original: vec![false; rows.len()],
            rows,
            rules: Vec::new(),
            action_qualification_count: None,
        });
    }
    ensure_not_recursive(&table, event)?;
    let columns = rule_columns(context.analysis, &table)?;
    let privilege_subject = context.security.privilege_subject(&table)?;
    let mut suppress_original = vec![false; rows.len()];
    let mut prepared_rules = Vec::with_capacity(rules.len());
    for rule in rules {
        let mut prepared = prepare_rule_actions(context, rule, &columns)?;
        for (index, row) in rows.iter_mut().enumerate() {
            if rule_condition_matches(
                context,
                &prepared.rule,
                &privilege_subject,
                index,
                row,
                &columns,
                &mut project,
            )? {
                prepared.matching_rows.push(index);
                if prepared.rule.definition.instead {
                    suppress_original[index] = true;
                }
            }
        }
        prepared_rules.push(prepared);
    }
    Ok(PreparedRuleBatch {
        table,
        event,
        rows,
        rules: prepared_rules,
        suppress_original,
        action_qualification_count: None,
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

struct ProjectedRuntimeRuleResolver<'a, F> {
    row_index: usize,
    row: &'a mut RuleRowImage,
    columns: &'a BTreeMap<String, RuleColumnMetadata>,
    project: &'a mut F,
}

impl<F> ProjectedRuntimeRuleResolver<'_, F>
where
    F: FnMut(usize, RuleRowSide, &str) -> Result<Option<Value>, SQLError>,
{
    fn record_field(
        &mut self,
        side: RuleRowSide,
        column: &str,
    ) -> Result<ResolvedVariable, SQLError> {
        let metadata = self
            .columns
            .get(column)
            .ok_or_else(|| SQLError::UnknownColumn(column.to_string()))?;
        let (record, doc_id) = match side {
            RuleRowSide::Old => (&mut self.row.old, self.row.old_doc_id),
            RuleRowSide::New => (&mut self.row.new, self.row.new_doc_id),
        };
        let value = if let Some(value) = record
            .as_ref()
            .and_then(|record| record.get(column).cloned())
        {
            value
        } else if metadata.uses_document_id {
            doc_id
                .map(i64::try_from)
                .transpose()
                .map_err(|_| {
                    SQLError::TypeMismatch("document id exceeds PostgreSQL bigint".into())
                })?
                .map_or(Value::Null, Value::Int)
        } else if record.is_some() {
            let value = (self.project)(self.row_index, side, column)?.unwrap_or(Value::Null);
            if let Some(record) = record.as_mut() {
                record.insert(column.to_string(), value.clone());
            }
            value
        } else {
            Value::Null
        };
        Ok(ResolvedVariable {
            value,
            declared_type: Some(metadata.ty.sql_name()),
        })
    }

    fn record(&mut self, side: RuleRowSide) -> Result<ResolvedVariable, SQLError> {
        let mut columns = self
            .columns
            .iter()
            .map(|(column, metadata)| (column.clone(), metadata.position))
            .collect::<Vec<_>>();
        columns.sort_by_key(|(_, position)| *position);
        let fields = columns
            .into_iter()
            .map(|(column, _)| {
                self.record_field(side, &column)
                    .map(|field| (column, field.value))
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(ResolvedVariable::untyped(Value::Record(fields)))
    }
}

impl<F> VariableResolver for ProjectedRuntimeRuleResolver<'_, F>
where
    F: FnMut(usize, RuleRowSide, &str) -> Result<Option<Value>, SQLError>,
{
    fn resolve_name(&mut self, name: &str) -> Result<Option<ResolvedVariable>, SQLError> {
        if name.eq_ignore_ascii_case("old") {
            return self.record(RuleRowSide::Old).map(Some);
        }
        if name.eq_ignore_ascii_case("new") {
            return self.record(RuleRowSide::New).map(Some);
        }
        Ok(None)
    }

    fn resolve_qualified(
        &mut self,
        qualifier: &str,
        column: &str,
    ) -> Result<Option<ResolvedVariable>, SQLError> {
        if qualifier.eq_ignore_ascii_case("old") {
            return self.record_field(RuleRowSide::Old, column).map(Some);
        }
        if qualifier.eq_ignore_ascii_case("new") {
            return self.record_field(RuleRowSide::New, column).map(Some);
        }
        Ok(None)
    }

    fn resolve_param(&mut self, _index: usize) -> Result<Option<ResolvedVariable>, SQLError> {
        Ok(None)
    }

    fn rewrite_qualified_whole_row(&mut self, qualifier: &str) -> Result<Option<Expr>, SQLError> {
        Ok(self
            .resolve_name(qualifier)?
            .map(|record| Expr::Literal(record.value)))
    }
}

mod condition_binding;
use condition_binding::rule_condition_matches;

pub mod views;

fn prepare_rule_actions(
    context: RuleContext<'_>,
    rule: uqa_sql::catalog::events::StoredRule,
    columns: &BTreeMap<String, RuleColumnMetadata>,
) -> Result<PreparedRule, SQLError> {
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
        matching_rows: Vec::new(),
        condition_references_row,
        action_references_row,
        action_row_columns,
        action_columns,
    })
}
