//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Recompute stored generated values from a relation's declared row schema.

use uqa_core::Value;
use uqa_sql::assignment::{
    conversion::convert_value_to_column_type_with_context, AssignmentContext,
};
use uqa_sql::{
    ast::{ColumnDef, Expr, GeneratedColumnKind},
    ResultRow, RowSchema, SQLError,
};

/// Recompute the selected stored generated columns (all when no selection is supplied); each value takes its column type through catalog-aware assignment conversion.
pub fn refresh_stored_generated_columns(
    assignment: &dyn AssignmentContext,
    columns: &[ColumnDef],
    selected: Option<&[String]>,
    document: &mut ResultRow,
    evaluate: &mut dyn FnMut(&Expr, &ResultRow, &RowSchema) -> Result<Value, SQLError>,
) -> Result<(), SQLError> {
    for column in columns {
        if column.generated.as_ref().is_some_and(|generated| {
            generated.kind == GeneratedColumnKind::Virtual
                || selected.is_none_or(|selected| selected.contains(&column.name))
        }) {
            document.remove(&column.name);
        }
    }
    let schema = RowSchema::with_types(
        columns.iter().map(|column| column.name.clone()).collect(),
        columns
            .iter()
            .map(|column| Some(column.ty.clone()))
            .collect(),
    );
    for column in columns {
        let Some(generated) = column.generated.as_ref() else {
            continue;
        };
        if generated.kind != GeneratedColumnKind::Stored
            || selected.is_some_and(|selected| !selected.contains(&column.name))
        {
            continue;
        }
        let value = evaluate(&generated.expression, document, &schema)?;
        document.insert(
            column.name.clone(),
            convert_value_to_column_type_with_context(assignment, value, &column.ty)?,
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests;
