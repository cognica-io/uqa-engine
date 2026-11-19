//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Source metadata traversal without rewriting stored query expressions.

use super::{FromClause, SQLError, SourceCallback, Statement};

struct Selected<F>(F);
impl<W, F: FnMut(&mut FromClause) -> Result<(), SQLError>> super::Actions<W> for Selected<F> {
    fn source(&mut self, value: &mut FromClause) -> Result<(), SQLError> {
        (self.0)(value)
    }
}
pub fn visit_stored_statement_sources(
    statement: &mut Statement,
    visit: SourceCallback<'_>,
) -> Result<(), SQLError> {
    crate::catalog::work::inline(visit_stored_statement_sources_from(
        statement,
        visit,
        &uqa_core::memory::ProductionControl::uncontrolled(),
        &mut (),
    ))
}
pub(crate) async fn visit_stored_statement_sources_from<W: super::Work + Send>(
    statement: &mut Statement,
    visit: impl FnMut(&mut FromClause) -> Result<(), SQLError>,
    control: &uqa_core::memory::ProductionControl<'_>,
    work: &mut W,
) -> Result<(), SQLError> {
    super::Reader::new(&mut Selected(visit), control, work)
        .bind_statement(statement)
        .await
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
