//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Source metadata traversal without rewriting stored query expressions.

use super::{FromClause, SQLError, SourceCallback, Statement, StoredAstVisitor};

pub fn visit_stored_statement_sources(
    statement: &mut Statement,
    visit: SourceCallback<'_>,
) -> Result<(), SQLError> {
    StoredAstVisitor {
        source: Some(visit),
        merge: None,
        expression: None,
        ty: None,
        relation: &mut |_: &mut String| Ok(()),
        routine: &mut |_: &mut String, _: Option<&mut Option<crate::ast::FunctionBinding>>| Ok(()),
    }
    .bind_statement(statement)
}

enum SourceColumnShape {
    Table(Vec<String>, Option<Vec<String>>),
    Join(Vec<String>),
}

/// Apply only planned source aliases and physical columns. The column-dependency binder's expression normalization is analysis state, not a replacement for the stored query.
pub fn copy_stored_source_column_shapes(
    planned: &mut Statement,
    destination: &mut Statement,
) -> Result<(), SQLError> {
    let mut shapes = Vec::new();
    visit_stored_statement_sources(planned, &mut |source| {
        match source {
            FromClause::Table {
                column_aliases,
                bound_columns,
                ..
            } => {
                shapes.push(SourceColumnShape::Table(
                    column_aliases.clone(),
                    bound_columns.clone(),
                ));
            }
            FromClause::Join { column_aliases, .. } => {
                shapes.push(SourceColumnShape::Join(column_aliases.clone()));
            }
            _ => {}
        }
        Ok(())
    })?;
    let mut shapes = shapes.into_iter();
    visit_stored_statement_sources(destination, &mut |source| {
        match source {
            FromClause::Table {
                column_aliases,
                bound_columns,
                ..
            } => {
                let Some(SourceColumnShape::Table(aliases, columns)) = shapes.next() else {
                    return Err(source_shape_mismatch());
                };
                *column_aliases = aliases;
                *bound_columns = columns;
            }
            FromClause::Join { column_aliases, .. } => {
                let Some(SourceColumnShape::Join(aliases)) = shapes.next() else {
                    return Err(source_shape_mismatch());
                };
                *column_aliases = aliases;
            }
            _ => {}
        }
        Ok(())
    })?;
    if shapes.next().is_some() {
        return Err(source_shape_mismatch());
    }
    Ok(())
}

fn source_shape_mismatch() -> SQLError {
    SQLError::Internal("stored source traversal changed while applying column aliases".into())
}
