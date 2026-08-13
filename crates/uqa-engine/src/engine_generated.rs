//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Physical/logical row conversion for `PostgreSQL` generated columns.

use uqa_sql::ast::{ColumnDef, GeneratedColumnKind};
use uqa_sql::expr::EvalContext;
use uqa_sql::{ResultRow, SQLError};
use uqa_storage::document_store::Document;

pub(crate) fn materialize_virtual_generated_columns(
    columns: &[ColumnDef],
    document: &mut Document,
) -> Result<(), SQLError> {
    materialize_matching_virtual_generated_columns(columns, document, |_| true)
}

pub(crate) fn materialize_selected_virtual_generated_columns(
    columns: &[ColumnDef],
    document: &mut Document,
    selected: &std::collections::BTreeSet<String>,
) -> Result<(), SQLError> {
    materialize_matching_virtual_generated_columns(columns, document, |name| {
        selected.contains(name)
    })
}

pub(crate) fn materialize_projected_virtual_generated_columns(
    columns: &[ColumnDef],
    document: &mut Document,
    projection: &[String],
) -> Result<(), SQLError> {
    materialize_matching_virtual_generated_columns(columns, document, |name| {
        projection.iter().any(|projected| projected == name)
    })
}

fn materialize_matching_virtual_generated_columns(
    columns: &[ColumnDef],
    document: &mut Document,
    mut selected: impl FnMut(&str) -> bool,
) -> Result<(), SQLError> {
    for column in columns {
        let Some(generated) = column.generated.as_ref() else {
            continue;
        };
        if generated.kind != GeneratedColumnKind::Virtual {
            continue;
        }
        if !selected(&column.name) {
            continue;
        }
        let row: &ResultRow = document;
        let value = uqa_sql::expr::eval(&generated.expression, &EvalContext::new(Some(row), &[]))?;
        document.insert(
            column.name.clone(),
            crate::sql::convert_value_to_column_type(value, &column.ty)?,
        );
    }
    Ok(())
}

pub(crate) fn strip_virtual_generated_columns(columns: &[ColumnDef], document: &mut Document) {
    for column in columns {
        if column
            .generated
            .as_ref()
            .is_some_and(|generated| generated.kind == GeneratedColumnKind::Virtual)
        {
            document.remove(&column.name);
        }
    }
}

pub(crate) fn projection_contains_virtual_generated_column(
    columns: &[ColumnDef],
    projection: &[String],
) -> bool {
    columns.iter().any(|column| {
        column
            .generated
            .as_ref()
            .is_some_and(|generated| generated.kind == GeneratedColumnKind::Virtual)
            && projection.iter().any(|name| name == &column.name)
    })
}
