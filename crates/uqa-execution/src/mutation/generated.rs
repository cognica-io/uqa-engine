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

/// Recompute every stored generated column; each value takes its column type through catalog-aware assignment conversion.
pub fn refresh_stored_generated_columns(
    assignment: &dyn AssignmentContext,
    columns: &[ColumnDef],
    document: &mut ResultRow,
    evaluate: &mut dyn FnMut(&Expr, &ResultRow, &RowSchema) -> Result<Value, SQLError>,
) -> Result<(), SQLError> {
    for column in columns {
        if column.generated.is_some() {
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
        if generated.kind != GeneratedColumnKind::Stored {
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
