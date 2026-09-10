//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Materialize the exact key set of a decorrelated EXISTS query.

use super::{QueryOutput, QueryRows};
use crate::query::projection::{close_after_physical_failure, physical_exec_error};
use crate::query::scope::subqueries::DirectColumnKey;
use crate::{RowSchemaExecution, SharedExpressionEvaluator};
use smallvec::SmallVec;
use uqa_core::Value;
use uqa_sql::{plan::ProjectionPlan, SQLError};

pub fn collect_exists_key_operator<'a>(
    columns: Vec<String>,
    mut operator: Box<dyn crate::PhysicalOperator + 'a>,
    projections: &[ProjectionPlan],
    evaluator: SharedExpressionEvaluator<'a>,
) -> Result<QueryOutput, SQLError> {
    let internal_columns = operator.schema().to_vec();
    let internal_types = operator.row_schema().column_types().to_vec();
    let column_types = projections
        .iter()
        .map(|projection| {
            crate::scalar_type(
                &projection.expr,
                operator.row_schema(),
                evaluator.parameters(),
            )
            .ok()
            .flatten()
        })
        .collect();
    let direct_columns = projections
        .iter()
        .map(|projection| DirectColumnKey::compile(&projection.expr))
        .collect::<Option<Vec<_>>>();
    let mut keys = crate::CanonicalRowHashSet::new();
    if let Err(error) = operator.open() {
        return Err(close_after_physical_failure(
            operator.as_mut(),
            error,
            "open EXISTS key input",
        ));
    }
    loop {
        let batch = match operator.next() {
            Ok(batch) => batch,
            Err(error) => {
                return Err(close_after_physical_failure(
                    operator.as_mut(),
                    error,
                    "collect EXISTS key input",
                ));
            }
        };
        let Some(batch) = batch else {
            break;
        };
        for row in &batch.rows {
            let view = batch.schema.view(row);
            let inserted = if let Some(direct_columns) = direct_columns.as_ref() {
                insert_direct_key(&mut keys, direct_columns, &view)
            } else {
                let mut key = SmallVec::<[Value; 4]>::with_capacity(projections.len());
                let mut contains_null = false;
                for projection in projections {
                    let value =
                        match evaluator.evaluate_physical(&projection.expr, &batch.schema, row) {
                            Ok(value) => value,
                            Err(error) => {
                                return Err(close_after_physical_failure(
                                    operator.as_mut(),
                                    error,
                                    "evaluate EXISTS key",
                                ));
                            }
                        };
                    if matches!(value, Value::Null) {
                        contains_null = true;
                        break;
                    }
                    key.push(value);
                }
                if contains_null {
                    Ok(false)
                } else {
                    keys.insert_values(&key)
                }
            };
            if let Err(error) = inserted {
                return Err(close_after_physical_failure(
                    operator.as_mut(),
                    error,
                    "hash EXISTS key",
                ));
            }
        }
    }
    operator.close().map_err(physical_exec_error)?;
    Ok(QueryOutput {
        internal_columns,
        internal_types,
        column_types,
        columns,
        rows: QueryRows::ExistsKeySet(keys),
    })
}

fn insert_direct_key(
    keys: &mut crate::CanonicalRowHashSet,
    columns: &[DirectColumnKey],
    row: &dyn uqa_sql::expr::RowLookup,
) -> crate::ExecResult<bool> {
    let mut key = SmallVec::<[&Value; 4]>::with_capacity(columns.len());
    for column in columns {
        let Some(value) = column.value(row) else {
            return Ok(false);
        };
        if matches!(value, Value::Null) {
            return Ok(false);
        }
        key.push(value);
    }
    keys.insert_borrowed(&key)
}
