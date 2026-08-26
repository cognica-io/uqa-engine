//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Set-oriented rewrite-rule action binding and internal OLD/NEW row sources.

use std::collections::BTreeMap;

use uqa_core::Value;
use uqa_sql::ast::{Expr, FromClause, JoinKind, RuleEvent, SelectStmt, Statement};
use uqa_sql::plpgsql::{ResolvedVariable, VariableResolver};
use uqa_sql::SQLError;

use crate::Engine;

use super::{RuleColumnMetadata, RuleRowImage, RuntimeRuleResolver};

pub(super) fn bind_insert_values_action(
    engine: &Engine,
    event: RuleEvent,
    action: &Statement,
    matching_rows: &[usize],
    rows: &[RuleRowImage],
    columns: &BTreeMap<String, RuleColumnMetadata>,
) -> Result<Statement, SQLError> {
    let action_columns = super::rule_action_target_columns(engine, action)?;
    let mut combined = None;
    for row_index in matching_rows {
        let row = rows.get(*row_index).ok_or_else(|| {
            SQLError::Internal("rewrite rule lost its qualified row image".into())
        })?;
        let bound = crate::engine_events::bind_rule_action(
            action,
            event,
            &action_columns,
            &mut runtime_rule_resolver(row, columns),
        )?;
        let Statement::Insert(mut insert) = bound else {
            return Err(SQLError::Internal(
                "rewrite rule INSERT VALUES action changed statement kind".into(),
            ));
        };
        if let Some(Statement::Insert(existing)) = combined.as_mut() {
            existing.rows.append(&mut insert.rows);
        } else {
            combined = Some(Statement::Insert(insert));
        }
    }
    combined.ok_or_else(|| SQLError::Internal("rewrite rule action lost its row source".into()))
}

fn runtime_rule_resolver<'a>(
    row: &'a RuleRowImage,
    columns: &'a BTreeMap<String, RuleColumnMetadata>,
) -> RuntimeRuleResolver<'a> {
    RuntimeRuleResolver {
        old: row.old.as_ref(),
        new: row.new.as_ref(),
        old_doc_id: row.old_doc_id,
        new_doc_id: row.new_doc_id,
        columns,
    }
}

struct RuleRowSource {
    clause: FromClause,
    qualifier: String,
    old_columns: BTreeMap<String, String>,
    new_columns: BTreeMap<String, String>,
    row_index_column: String,
}

pub(super) struct BoundSetOrientedAction {
    pub(super) statement: Statement,
    pub(super) source_index: Expr,
}

pub(super) fn bind_set_oriented_action(
    engine: &Engine,
    event: RuleEvent,
    action: &Statement,
    matching_rows: &[usize],
    rows: &[RuleRowImage],
    columns: &BTreeMap<String, RuleColumnMetadata>,
    qualifier: &str,
) -> Result<BoundSetOrientedAction, SQLError> {
    let source = rule_row_source(matching_rows, rows, columns, qualifier)?;
    let source_index = Expr::qualified_column(&source.qualifier, &source.row_index_column);
    let action_columns = super::rule_action_target_columns(engine, action)?;
    let mut bound = crate::engine_events::bind_rule_action(
        action,
        event,
        &action_columns,
        &mut RuleSourceResolver {
            qualifier: &source.qualifier,
            old_columns: &source.old_columns,
            new_columns: &source.new_columns,
        },
    )?;
    attach_rule_row_source(&mut bound, source.clause)?;
    Ok(BoundSetOrientedAction {
        statement: bound,
        source_index,
    })
}

fn rule_row_source(
    matching_rows: &[usize],
    rows: &[RuleRowImage],
    columns: &BTreeMap<String, RuleColumnMetadata>,
    qualifier: &str,
) -> Result<RuleRowSource, SQLError> {
    let row_index_column = "__row_index".to_string();
    let mut column_aliases = Vec::with_capacity(columns.len() * 2 + 1);
    let mut old_columns = BTreeMap::new();
    let mut new_columns = BTreeMap::new();
    for (index, column) in columns.keys().enumerate() {
        let old_alias = format!("__old_{index}");
        let new_alias = format!("__new_{index}");
        column_aliases.push(old_alias.clone());
        column_aliases.push(new_alias.clone());
        old_columns.insert(column.clone(), old_alias);
        new_columns.insert(column.clone(), new_alias);
    }
    column_aliases.push(row_index_column.clone());
    let values = matching_rows
        .iter()
        .map(|row_index| {
            let row = rows.get(*row_index).ok_or_else(|| {
                SQLError::Internal("rewrite rule lost its qualified row image".into())
            })?;
            let resolver = runtime_rule_resolver(row, columns);
            let mut values = Vec::with_capacity(column_aliases.len());
            for column in columns.keys() {
                values.push(resolved_variable_expr(resolver.record_field(
                    row.old.as_ref(),
                    row.old_doc_id,
                    column,
                )?));
                values.push(resolved_variable_expr(resolver.record_field(
                    row.new.as_ref(),
                    row.new_doc_id,
                    column,
                )?));
            }
            let row_index = i64::try_from(*row_index).map_err(|_| {
                SQLError::Internal("rewrite rule event row index exceeds BIGINT".into())
            })?;
            values.push(Expr::Literal(Value::Int(row_index)));
            Ok(values)
        })
        .collect::<Result<Vec<_>, SQLError>>()?;
    Ok(RuleRowSource {
        clause: FromClause::Values {
            rows: values,
            alias: Some(qualifier.to_string()),
            column_aliases,
        },
        qualifier: qualifier.to_string(),
        old_columns,
        new_columns,
        row_index_column,
    })
}

fn resolved_variable_expr(variable: ResolvedVariable) -> Expr {
    let ResolvedVariable {
        value,
        declared_type,
    } = variable;
    match declared_type {
        Some(ty) => Expr::Cast {
            expr: Box::new(Expr::Literal(value)),
            ty,
        },
        None => Expr::Literal(value),
    }
}

struct RuleSourceResolver<'a> {
    qualifier: &'a str,
    old_columns: &'a BTreeMap<String, String>,
    new_columns: &'a BTreeMap<String, String>,
}

impl VariableResolver for RuleSourceResolver<'_> {
    fn resolve_name(&mut self, _name: &str) -> Result<Option<ResolvedVariable>, SQLError> {
        Ok(None)
    }

    fn resolve_qualified(
        &mut self,
        _qualifier: &str,
        _column: &str,
    ) -> Result<Option<ResolvedVariable>, SQLError> {
        Ok(None)
    }

    fn resolve_param(&mut self, _index: usize) -> Result<Option<ResolvedVariable>, SQLError> {
        Ok(None)
    }

    fn rewrite_qualified(
        &mut self,
        qualifier: &str,
        column: &str,
    ) -> Result<Option<Expr>, SQLError> {
        let columns = if qualifier.eq_ignore_ascii_case("old") {
            self.old_columns
        } else if qualifier.eq_ignore_ascii_case("new") {
            self.new_columns
        } else {
            return Ok(None);
        };
        let source_column = columns
            .get(column)
            .ok_or_else(|| SQLError::UnknownColumn(format!("{qualifier}.{column}")))?;
        Ok(Some(Expr::qualified_column(self.qualifier, source_column)))
    }
}

fn attach_rule_row_source(statement: &mut Statement, source: FromClause) -> Result<(), SQLError> {
    match statement {
        Statement::Select(select) => attach_select_rule_source(select, &source),
        Statement::Insert(insert) => {
            let select = insert.select_source.as_mut().ok_or_else(|| {
                SQLError::Internal("set-oriented rule INSERT action has no SELECT source".into())
            })?;
            attach_select_rule_source(select, &source);
        }
        Statement::Update(update) => {
            update.from = Some(cross_join_source(update.from.take(), source));
        }
        Statement::Delete(delete) => {
            delete.using = Some(cross_join_source(delete.using.take(), source));
        }
        _ => {
            return Err(SQLError::Internal(
                "validated rewrite-rule action changed statement kind".into(),
            ))
        }
    }
    Ok(())
}

fn attach_select_rule_source(select: &mut SelectStmt, source: &FromClause) {
    if let Some(set_op) = select.set_op.as_mut() {
        if let Some(left) = set_op.left.as_mut() {
            attach_select_rule_source(left, source);
        } else {
            select.from = Some(cross_join_source(select.from.take(), source.clone()));
        }
        attach_select_rule_source(&mut set_op.right, source);
    } else {
        select.from = Some(cross_join_source(select.from.take(), source.clone()));
    }
}

fn cross_join_source(existing: Option<FromClause>, source: FromClause) -> FromClause {
    let Some(existing) = existing else {
        return source;
    };
    FromClause::Join {
        left: Box::new(existing),
        right: Box::new(source),
        kind: JoinKind::Cross,
        on: None,
        using: None,
        natural: false,
        alias: None,
        column_aliases: Vec::new(),
        lateral: false,
    }
}
