//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Physical/logical row conversion for `PostgreSQL` generated columns.

use uqa_sql::ast::{ColumnDef, GeneratedColumnKind};
use uqa_sql::{ResultRow, SQLError};
use uqa_storage::document_store::Document;

pub fn materialize_virtual_generated_columns(
    columns: &[ColumnDef],
    document: &mut Document,
) -> Result<(), SQLError> {
    materialize_matching_virtual_generated_columns(columns, document, |_| true)
}

pub fn materialize_selected_virtual_generated_columns(
    columns: &[ColumnDef],
    document: &mut Document,
    selected: &std::collections::BTreeSet<String>,
) -> Result<(), SQLError> {
    materialize_matching_virtual_generated_columns(columns, document, |name| {
        selected.contains(name)
    })
}

pub fn materialize_projected_virtual_generated_columns(
    columns: &[ColumnDef],
    document: &mut Document,
    projection: &[String],
) -> Result<(), SQLError> {
    materialize_matching_virtual_generated_columns(columns, document, |name| {
        projection.iter().any(|projected| projected == name)
    })
}

pub fn materialize_missing_generated_columns(
    columns: &[ColumnDef],
    document: &mut Document,
) -> Result<(), SQLError> {
    let schema = crate::RowSchema::with_types(
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
        if document.contains_key(&column.name) {
            continue;
        }
        let value = evaluate_generated_column(&schema, generated, document)?;
        document.insert(
            column.name.clone(),
            uqa_sql::assignment::conversion::convert_value_to_column_type(value, &column.ty)?,
        );
    }
    Ok(())
}

fn materialize_matching_virtual_generated_columns(
    columns: &[ColumnDef],
    document: &mut Document,
    mut selected: impl FnMut(&str) -> bool,
) -> Result<(), SQLError> {
    let schema = crate::RowSchema::with_types(
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
        if generated.kind != GeneratedColumnKind::Virtual {
            continue;
        }
        if !selected(&column.name) {
            continue;
        }
        let value = evaluate_generated_column(&schema, generated, document)?;
        document.insert(
            column.name.clone(),
            uqa_sql::assignment::conversion::convert_value_to_column_type(value, &column.ty)?,
        );
    }
    Ok(())
}

fn evaluate_generated_column(
    schema: &crate::RowSchema,
    generated: &uqa_sql::ast::GeneratedColumn,
    document: &Document,
) -> Result<uqa_core::Value, SQLError> {
    let mut expression = uqa_sql::plan::ExpressionPlan::lower((*generated.expression).clone());
    if !expression.subqueries.is_empty() {
        return Err(SQLError::Internal(
            "validated generated expression contains a subquery".into(),
        ));
    }
    expression.scalar = crate::bind_type_introspection(expression.scalar, schema, &[]);
    let row: &ResultRow = document;
    crate::eval_scalar(
        &expression.scalar,
        &crate::ScalarEvalContext::new(Some(row), &[]),
    )
}

pub fn strip_virtual_generated_columns(columns: &[ColumnDef], document: &mut Document) {
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

pub fn projection_contains_virtual_generated_column(
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
