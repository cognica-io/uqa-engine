//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Apply the durable input shape of stored table references to current table scans.

use uqa_execution::{ColumnSelection, PhysicalOperator};
use uqa_sql::SQLError;

pub(in crate::sql) use uqa_sql::semantics::bound_source_column_names;

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
