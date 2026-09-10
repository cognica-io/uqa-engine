//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Recompute stored generated values from a relation's declared row schema.

use uqa_core::Value;
use uqa_sql::assignment::conversion::convert_value_to_column_type;
use uqa_sql::{
    ast::{ColumnDef, Expr, GeneratedColumnKind},
    ResultRow, RowSchema, SQLError,
};

pub fn refresh_stored_generated_columns(
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
            convert_value_to_column_type(value, &column.ty)?,
        );
    }
    Ok(())
}
