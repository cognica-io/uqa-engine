//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Scope and alias helpers for durable rewrite-rule column binding.

use std::collections::BTreeSet;

use uqa_sql::ast::{Expr, FromClause, SelectStmt};
use uqa_sql::SQLError;

use crate::{Engine, RelationIdentity};

use super::{ColumnScope, ScopeColumn};

pub(super) fn preserve_table_column_name(
    engine: &Engine,
    source: &mut FromClause,
    relation: &RelationIdentity,
    from: &str,
    to: &str,
) -> Result<bool, SQLError> {
    match source {
        FromClause::Table {
            name,
            qualifier,
            alias,
            column_aliases,
            ..
        } => {
            let identity = RelationIdentity::from_legacy_name(name).map_err(|error| {
                SQLError::Internal(format!("decode bound rule relation `{name}`: {error}"))
            })?;
            if &identity != relation {
                return Ok(false);
            }
            let columns = crate::sql::query_source_column_names(engine, name, true)?
                .ok_or_else(|| SQLError::UnknownTable(name.clone()))?;
            let position = columns
                .iter()
                .position(|column| same_identifier(column, to))
                .ok_or_else(|| {
                    SQLError::Internal(format!(
                        "renamed rule dependency column \"{to}\" is missing from relation `{name}`"
                    ))
                })?;
            if let Some(visible) = column_aliases.get(position) {
                return Ok(same_identifier(visible, from));
            }
            let existing = column_aliases.len();
            column_aliases.extend(columns.iter().skip(existing).cloned());
            column_aliases[position] = from.to_string();
            if alias.is_none() {
                *alias = Some(qualifier.clone());
            }
            Ok(true)
        }
        FromClause::Join {
            left, right, alias, ..
        } if alias.is_none() => {
            let left_changed = preserve_table_column_name(engine, left, relation, from, to)?;
            let right_changed = preserve_table_column_name(engine, right, relation, from, to)?;
            Ok(left_changed || right_changed)
        }
        FromClause::Join { .. }
        | FromClause::Values { .. }
        | FromClause::Function { .. }
        | FromClause::FunctionGroup { .. }
        | FromClause::Subquery { .. } => Ok(false),
    }
}

pub(super) fn table_alias_count_error(
    qualifier: &str,
    available: usize,
    specified: usize,
) -> SQLError {
    SQLError::Routine {
        sqlstate: "42P10".into(),
        message: format!(
            "table \"{qualifier}\" has {available} columns available but {specified} columns specified"
        ),
    }
}

pub(super) fn opaque_scope(columns: &[String], qualifier: Option<&str>) -> ColumnScope {
    let output = columns
        .iter()
        .map(|name| ScopeColumn {
            name: name.clone(),
            current_name: name.clone(),
            reference: qualifier.map_or_else(
                || Expr::Column(name.clone()),
                |qualifier| Expr::qualified_column(qualifier, name),
            ),
            dependencies: BTreeSet::new(),
        })
        .collect::<Vec<_>>();
    let mut scope = ColumnScope {
        output: output.clone(),
        ..ColumnScope::default()
    };
    if let Some(qualifier) = qualifier {
        scope.insert_qualifier(qualifier, &output);
    }
    scope
}

pub(super) fn action_returning_scope(
    local: &ColumnScope,
    target: &ColumnScope,
    event: uqa_sql::ast::RuleEvent,
    aliases: &uqa_sql::ast::ReturningAliases,
) -> ColumnScope {
    let mut scope = local.clone();
    let expose_old = event == uqa_sql::ast::RuleEvent::Insert || aliases.old_explicit;
    let expose_new = event == uqa_sql::ast::RuleEvent::Insert || aliases.new_explicit;
    if expose_old {
        scope.insert_qualifier(&aliases.old, &target.output);
    }
    if expose_new {
        scope.insert_qualifier(&aliases.new, &target.output);
    }
    scope
}

pub(super) fn select_output_names(select: &SelectStmt) -> Vec<String> {
    if let Some(left) = select.set_op.as_ref().and_then(|set| set.left.as_deref()) {
        return select_output_names(left);
    }
    if select.projections.is_empty() && !select.values.is_empty() {
        return (1..=select.values.first().map_or(0, Vec::len))
            .map(|position| format!("column{position}"))
            .collect();
    }
    select
        .projections
        .iter()
        .map(|projection| {
            projection
                .alias
                .clone()
                .unwrap_or_else(|| match &projection.expr {
                    Expr::Column(name) | Expr::QualifiedColumn { column: name, .. } => name.clone(),
                    Expr::Func { name, .. } => name
                        .rsplit_once('.')
                        .map_or(name.as_str(), |(_, local)| local)
                        .to_string(),
                    _ => "?column?".into(),
                })
        })
        .collect()
}

pub(super) fn apply_positional_aliases(columns: &mut Vec<String>, aliases: &[String]) {
    if columns.is_empty() {
        columns.extend_from_slice(aliases);
        return;
    }
    for (column, alias) in columns.iter_mut().zip(aliases) {
        column.clone_from(alias);
    }
}

pub(super) fn unique_current_name(columns: &[&ScopeColumn]) -> Option<String> {
    let [column] = columns else {
        return None;
    };
    Some(column.current_name.clone())
}

pub(super) fn is_output_alias(expression: &Expr, names: &[String]) -> bool {
    let Expr::Column(name) = expression else {
        return false;
    };
    names
        .iter()
        .any(|candidate| same_identifier(candidate, name))
}

pub(super) fn is_default_values_insert(rows: &[Vec<Expr>]) -> bool {
    matches!(rows, [row] if row.is_empty())
}

pub(super) fn same_identifier(left: &str, right: &str) -> bool {
    left == right || left.eq_ignore_ascii_case(right)
}
