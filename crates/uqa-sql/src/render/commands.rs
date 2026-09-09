//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Rendering for data-modifying statement trees.

use super::{
    assignments_sql, expr_sql, from_sql, ident, ident_list, only_relation, render_returning,
    render_target_alias, rows_sql, select_sql, with_sql,
};
use crate::ast::{DeleteStmt, InsertStmt, MergeStmt, MergeWhen, OnConflictAction, UpdateStmt};

pub(super) fn insert_sql(statement: &InsertStmt) -> String {
    let mut rendered = with_sql(&statement.with);
    rendered.push_str("INSERT INTO ");
    rendered.push_str(&only_relation(
        &statement.table,
        statement.include_descendants,
    ));
    render_target_alias(&mut rendered, &statement.table, &statement.target_qualifier);
    if !statement.columns.is_empty() {
        rendered.push_str(" (");
        rendered.push_str(&ident_list(&statement.columns));
        rendered.push(')');
    }
    if statement.rows.as_slice() == [Vec::new()] {
        rendered.push_str(" DEFAULT VALUES");
    } else if !statement.rows.is_empty() {
        rendered.push_str(" VALUES ");
        rendered.push_str(&rows_sql(&statement.rows));
    } else if let Some(select) = statement.select_source.as_deref() {
        rendered.push(' ');
        rendered.push_str(&select_sql(select));
    }
    if let Some(conflict) = &statement.on_conflict {
        rendered.push_str(" ON CONFLICT");
        if let Some(constraint) = &conflict.constraint {
            rendered.push_str(" ON CONSTRAINT ");
            rendered.push_str(&ident(constraint));
        }
        if !conflict.conflict_columns.is_empty() || !conflict.expressions.is_empty() {
            rendered.push_str(" (");
            let keys = conflict
                .conflict_columns
                .iter()
                .map(|name| ident(name))
                .chain(
                    conflict
                        .expressions
                        .iter()
                        .map(|expr| format!("({})", expr_sql(expr))),
                )
                .collect::<Vec<_>>();
            rendered.push_str(&keys.join(", "));
            rendered.push(')');
        }
        if let Some(predicate) = &conflict.predicate {
            rendered.push_str(" WHERE ");
            rendered.push_str(&expr_sql(predicate));
        }
        match &conflict.action {
            OnConflictAction::Nothing => rendered.push_str(" DO NOTHING"),
            OnConflictAction::Update {
                assignments,
                r#where,
            } => {
                rendered.push_str(" DO UPDATE SET ");
                rendered.push_str(&assignments_sql(assignments));
                if let Some(predicate) = r#where {
                    rendered.push_str(" WHERE ");
                    rendered.push_str(&expr_sql(predicate));
                }
            }
        }
    }
    render_returning(
        &mut rendered,
        &statement.returning_aliases,
        &statement.returning,
    );
    rendered
}

pub(super) fn update_sql(statement: &UpdateStmt) -> String {
    let mut rendered = with_sql(&statement.with);
    rendered.push_str("UPDATE ");
    rendered.push_str(&only_relation(
        &statement.table,
        statement.include_descendants,
    ));
    render_target_alias(&mut rendered, &statement.table, &statement.target_qualifier);
    rendered.push_str(" SET ");
    rendered.push_str(&assignments_sql(&statement.assignments));
    if let Some(source) = &statement.from {
        rendered.push_str(" FROM ");
        rendered.push_str(&from_sql(source));
    }
    if let Some(predicate) = &statement.r#where {
        rendered.push_str(" WHERE ");
        rendered.push_str(&expr_sql(predicate));
    }
    render_returning(
        &mut rendered,
        &statement.returning_aliases,
        &statement.returning,
    );
    rendered
}

pub(super) fn delete_sql(statement: &DeleteStmt) -> String {
    let mut rendered = with_sql(&statement.with);
    rendered.push_str("DELETE FROM ");
    rendered.push_str(&only_relation(
        &statement.table,
        statement.include_descendants,
    ));
    render_target_alias(&mut rendered, &statement.table, &statement.target_qualifier);
    if let Some(source) = &statement.using {
        rendered.push_str(" USING ");
        rendered.push_str(&from_sql(source));
    }
    if let Some(predicate) = &statement.r#where {
        rendered.push_str(" WHERE ");
        rendered.push_str(&expr_sql(predicate));
    }
    render_returning(
        &mut rendered,
        &statement.returning_aliases,
        &statement.returning,
    );
    rendered
}

pub(super) fn merge_sql(statement: &MergeStmt) -> String {
    let mut rendered = with_sql(&statement.with);
    rendered.push_str("MERGE INTO ");
    rendered.push_str(&only_relation(
        &statement.target,
        statement.include_descendants,
    ));
    render_target_alias(
        &mut rendered,
        &statement.target,
        &statement.target_qualifier,
    );
    rendered.push_str(" USING ");
    rendered.push_str(&from_sql(&statement.source));
    rendered.push_str(" ON ");
    rendered.push_str(&expr_sql(&statement.join_condition));
    for clause in &statement.when_clauses {
        let (matching, condition) = match clause {
            MergeWhen::UpdateMatched { condition, .. }
            | MergeWhen::DeleteMatched { condition }
            | MergeWhen::NothingMatched { condition } => ("MATCHED", condition),
            MergeWhen::UpdateNotMatchedBySource { condition, .. }
            | MergeWhen::DeleteNotMatchedBySource { condition }
            | MergeWhen::NothingNotMatchedBySource { condition } => {
                ("NOT MATCHED BY SOURCE", condition)
            }
            MergeWhen::InsertNotMatched { condition, .. }
            | MergeWhen::NothingNotMatched { condition } => ("NOT MATCHED BY TARGET", condition),
        };
        rendered.push_str(" WHEN ");
        rendered.push_str(matching);
        if let Some(condition) = condition {
            rendered.push_str(" AND ");
            rendered.push_str(&expr_sql(condition));
        }
        rendered.push_str(" THEN ");
        match clause {
            MergeWhen::UpdateMatched { assignments, .. }
            | MergeWhen::UpdateNotMatchedBySource { assignments, .. } => {
                rendered.push_str("UPDATE SET ");
                rendered.push_str(&assignments_sql(assignments));
            }
            MergeWhen::DeleteMatched { .. } | MergeWhen::DeleteNotMatchedBySource { .. } => {
                rendered.push_str("DELETE");
            }
            MergeWhen::InsertNotMatched {
                columns, values, ..
            } => {
                rendered.push_str("INSERT");
                if !columns.is_empty() {
                    rendered.push_str(" (");
                    rendered.push_str(&ident_list(columns));
                    rendered.push(')');
                }
                if values.is_empty() {
                    rendered.push_str(" DEFAULT VALUES");
                } else {
                    rendered.push_str(" VALUES (");
                    rendered.push_str(&values.iter().map(expr_sql).collect::<Vec<_>>().join(", "));
                    rendered.push(')');
                }
            }
            MergeWhen::NothingMatched { .. }
            | MergeWhen::NothingNotMatched { .. }
            | MergeWhen::NothingNotMatchedBySource { .. } => rendered.push_str("DO NOTHING"),
        }
    }
    render_returning(
        &mut rendered,
        &statement.returning_aliases,
        &statement.returning,
    );
    rendered
}
