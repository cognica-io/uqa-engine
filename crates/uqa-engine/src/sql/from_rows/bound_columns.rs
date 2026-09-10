//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Apply the durable input shape of stored table references to current table scans.

use uqa_execution::{ColumnSelection, PhysicalOperator, RowSchema};
use uqa_sql::SQLError;

pub(in crate::sql) fn bound_source_column_names(
    current: Vec<String>,
    bound: Option<&[String]>,
) -> Result<Vec<String>, SQLError> {
    let Some(bound) = bound else {
        return Ok(current);
    };
    for name in bound {
        if !current.contains(name) {
            return Err(SQLError::UnknownColumn(name.clone()));
        }
    }
    Ok(bound.to_vec())
}

pub(in crate::sql) fn bound_source_schema(
    schema: &RowSchema,
    bound: Option<&[String]>,
) -> Result<RowSchema, SQLError> {
    let Some(bound) = bound else {
        return Ok(schema.clone());
    };
    let columns = bound_source_column_names(schema.columns().to_vec(), Some(bound))?;
    Ok(RowSchema::select(
        schema,
        &columns
            .into_iter()
            .map(|name| (name.clone(), name))
            .collect::<Vec<_>>(),
    ))
}

pub(in crate::sql) fn bound_source_operator<'a>(
    operator: Box<dyn PhysicalOperator + 'a>,
    bound: Option<&[String]>,
) -> Result<Box<dyn PhysicalOperator + 'a>, SQLError> {
    let Some(bound) = bound else {
        return Ok(operator);
    };
    if operator.schema() == bound {
        return Ok(operator);
    }
    let mapping = bound
        .iter()
        .map(|name| {
            let position = operator
                .schema()
                .iter()
                .position(|column| column == name)
                .ok_or_else(|| SQLError::UnknownColumn(name.clone()))?;
            Ok((
                name.clone(),
                operator.row_schema().identities()[position].clone(),
                position,
            ))
        })
        .collect::<Result<Vec<_>, SQLError>>()?;
    Ok(Box::new(ColumnSelection::with_identities(
        operator, mapping,
    )))
}
