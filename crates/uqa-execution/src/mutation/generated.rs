//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Recompute stored generated values from a relation's declared row schema.

use uqa_core::Value;
use uqa_sql::assignment::{columns::coerce_column_value, AssignmentContext};
use uqa_sql::{
    ast::{ColumnDef, Expr, GeneratedColumn, GeneratedColumnKind},
    ResultRow, RowSchema, SQLError,
};

/// Evaluate a generated expression without discarding its analyzed source type.
pub type GeneratedColumnEvaluator<'a> = dyn FnMut(&Expr, &ResultRow, &RowSchema) -> Result<(Value, Option<uqa_sql::ColumnType>), SQLError>
    + 'a;

/// Recompute the selected stored generated columns (all when no selection is supplied); each value takes its column type through catalog-aware assignment conversion.
pub fn refresh_stored_generated_columns(
    assignment: &dyn AssignmentContext,
    columns: &[ColumnDef],
    selected: Option<&[String]>,
    document: &mut ResultRow,
    evaluate: &mut GeneratedColumnEvaluator<'_>,
) -> Result<(), SQLError> {
    for column in columns {
        if column.generated.as_ref().is_some_and(|generated| {
            generated.kind == GeneratedColumnKind::Virtual
                || selected.is_none_or(|selected| selected.contains(&column.name))
        }) {
            document.remove(&column.name);
        }
    }
    if selected.is_some_and(<[String]>::is_empty) {
        return Ok(());
    }
    let schema = generated_row_schema(columns);
    for (column, generated) in stored_generation_expressions(columns, selected) {
        let (value, source) = evaluate(&generated.expression, document, &schema)?;
        document.insert(
            column.name.clone(),
            coerce_column_value(assignment, value, &column.ty, source.as_ref())?,
        );
    }
    Ok(())
}

pub(crate) fn generated_row_schema(columns: &[ColumnDef]) -> RowSchema {
    RowSchema::with_types(
        columns.iter().map(|column| column.name.clone()).collect(),
        columns
            .iter()
            .map(|column| Some(column.ty.clone()))
            .collect(),
    )
}

pub(crate) fn stored_generation_expressions<'a>(
    columns: &'a [ColumnDef],
    selected: Option<&'a [String]>,
) -> impl Iterator<Item = (&'a ColumnDef, &'a GeneratedColumn)> {
    columns.iter().filter_map(move |column| {
        let generated = column.generated.as_ref()?;
        (generated.kind == GeneratedColumnKind::Stored
            && selected.is_none_or(|selected| selected.contains(&column.name)))
        .then_some((column, generated))
    })
}

#[cfg(test)]
mod tests;
