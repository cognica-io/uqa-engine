//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Materialized, spilled, hash-key, and directional query result collection.

use super::{
    consumer::{QueryConsumerControl, QueryOutputMode, QueryRowConsumer},
    output::{QueryOutput, QueryRows},
    projection::{close_after_physical_failure, physical_exec_error, physical_work_mem_bytes},
    runtime::QueryRuntimeView,
};
use crate::RowSchemaExecution;
use smallvec::SmallVec;
use std::rc::Rc;
use uqa_core::Value;
use uqa_sql::SQLError;

#[expect(
    clippy::too_many_lines,
    reason = "preserves SELECT schema and row identity"
)]
pub fn collect_query_operator<'a>(
    runtime: QueryRuntimeView<'_>,
    columns: Vec<String>,
    mut operator: Box<dyn crate::PhysicalOperator + 'a>,
    output_mode: QueryOutputMode<'_>,
) -> Result<QueryOutput, SQLError> {
    let internal_schema = operator.row_schema().clone();
    let internal_columns = internal_schema.columns().to_vec();
    let internal_types = internal_schema.column_types().to_vec();
    let column_types = columns
        .iter()
        .enumerate()
        .map(|(index, column)| {
            if internal_schema.columns().get(index) == Some(column) {
                internal_schema.column_type(index).cloned()
            } else {
                internal_schema
                    .position(column)
                    .and_then(|position| internal_schema.column_type(position).cloned())
            }
        })
        .collect();
    let rows = match output_mode {
        QueryOutputMode::Rows => {
            let has_duplicate_labels = {
                let mut seen = std::collections::BTreeSet::new();
                columns.iter().any(|column| !seen.insert(column))
            };
            if has_duplicate_labels {
                let batches = crate::physical::run_to_batches(operator.as_mut())
                    .map_err(physical_exec_error)?;
                let mut named = Vec::new();
                let mut positional = Vec::new();
                for batch in batches {
                    let columnar = crate::ColumnarBatch::from_batch(&columns, batch.clone());
                    positional.extend(columnar.into_positional_rows());
                    named.extend(batch.into_result_rows());
                }
                QueryRows::Rows {
                    named,
                    positional: Some(positional),
                }
            } else {
                QueryRows::Rows {
                    named: crate::physical::run_to_rows(operator.as_mut())
                        .map_err(physical_exec_error)?
                        .1,
                    positional: None,
                }
            }
        }
        QueryOutputMode::SharedSpill => {
            let mut buffer = crate::SpillBuffer::new(physical_work_mem_bytes(runtime)?.max(1));
            if let Err(error) = operator.open() {
                return Err(close_after_physical_failure(
                    operator.as_mut(),
                    error,
                    "open",
                ));
            }
            loop {
                let batch = match operator.next() {
                    Ok(batch) => batch,
                    Err(error) => {
                        return Err(close_after_physical_failure(
                            operator.as_mut(),
                            error,
                            "execution",
                        ));
                    }
                };
                let Some(batch) = batch else {
                    break;
                };
                if let Err(error) = buffer.push(batch) {
                    return Err(close_after_physical_failure(
                        operator.as_mut(),
                        error,
                        "spill buffering",
                    ));
                }
            }
            operator.close().map_err(physical_exec_error)?;
            QueryRows::SharedSpill(
                buffer
                    .into_shared(internal_schema)
                    .map_err(physical_exec_error)?,
            )
        }
        QueryOutputMode::ExistsKeySet => {
            if operator.row_schema().len() < columns.len() {
                return Err(SQLError::Internal(format!(
                    "decorrelated EXISTS result has {} columns for {} keys",
                    operator.row_schema().len(),
                    columns.len()
                )));
            }
            let key_positions = (0..columns.len()).collect::<Vec<_>>();
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
                            "collect EXISTS keys",
                        ));
                    }
                };
                let Some(batch) = batch else {
                    break;
                };
                for row in &batch.rows {
                    let view = batch.schema.view(row);
                    let mut key = SmallVec::<[&Value; 4]>::with_capacity(key_positions.len());
                    let mut contains_null = false;
                    for position in &key_positions {
                        let Some(value) = view.value_at(*position) else {
                            contains_null = true;
                            break;
                        };
                        if matches!(value, Value::Null) {
                            contains_null = true;
                            break;
                        }
                        key.push(value);
                    }
                    if !contains_null {
                        if let Err(error) = keys.insert_borrowed(&key) {
                            return Err(close_after_physical_failure(
                                operator.as_mut(),
                                error,
                                "hash EXISTS keys",
                            ));
                        }
                    }
                }
            }
            operator.close().map_err(physical_exec_error)?;
            QueryRows::ExistsKeySet(keys)
        }
        QueryOutputMode::RowConsumer(consumer) => {
            if consumer.uses_directional_scan() {
                collect_directional_query_operator(&columns, &mut operator, &consumer)?;
                return Ok(QueryOutput {
                    columns,
                    column_types,
                    internal_columns,
                    internal_types,
                    rows: QueryRows::Rows {
                        named: Vec::new(),
                        positional: None,
                    },
                });
            }
            consumer.begin(&columns, &internal_schema)?;
            if let Err(error) = operator.open() {
                return Err(close_after_physical_failure(
                    operator.as_mut(),
                    error,
                    "open row consumer input",
                ));
            }
            'consume: loop {
                let batch = match operator.next() {
                    Ok(batch) => batch,
                    Err(error) => {
                        return Err(close_after_physical_failure(
                            operator.as_mut(),
                            error,
                            "execute row consumer input",
                        ));
                    }
                };
                let Some(batch) = batch else {
                    break;
                };
                let crate::Batch { schema, rows } = batch;
                for row in rows {
                    let row = crate::OwnedPhysicalRow::new(schema.clone(), row);
                    match consumer.consume(row) {
                        Ok(QueryConsumerControl::Continue) => {}
                        Ok(QueryConsumerControl::Stop) => break 'consume,
                        Ok(QueryConsumerControl::Rewind) => {
                            return Err(close_after_physical_failure(
                                operator.as_mut(),
                                crate::ExecError::Other(
                                    "forward-only row consumer requested rewind".into(),
                                ),
                                "consume query row",
                            ));
                        }
                        Err(error) => {
                            return Err(close_after_physical_failure(
                                operator.as_mut(),
                                crate::ExecError::SQL(error),
                                "consume query row",
                            ));
                        }
                    }
                }
            }
            operator.close().map_err(physical_exec_error)?;
            QueryRows::Rows {
                named: Vec::new(),
                positional: None,
            }
        }
    };
    Ok(QueryOutput {
        columns,
        column_types,
        internal_columns,
        internal_types,
        rows,
    })
}

fn collect_directional_query_operator(
    columns: &[String],
    operator: &mut Box<dyn crate::PhysicalOperator + '_>,
    consumer: &Rc<dyn QueryRowConsumer + '_>,
) -> Result<(), SQLError> {
    let support = operator.backward_scan_support();
    consumer.directional_scan_prepared(support)?;
    if support != crate::BackwardScanSupport::Native {
        let placeholder: Box<dyn crate::PhysicalOperator> = Box::new(
            crate::TableScan::from_physical_rows(operator.row_schema().clone(), Vec::new()),
        );
        let inner = std::mem::replace(operator, placeholder);
        *operator = Box::new(crate::ScrollMaterialize::new(inner));
    }
    consumer.begin(columns, operator.row_schema())?;
    if let Err(error) = operator.open() {
        return Err(close_after_physical_failure(
            operator.as_mut(),
            error,
            "open directional row consumer input",
        ));
    }
    loop {
        let batch = match operator.next_direction(consumer.scan_direction()) {
            Ok(batch) => batch,
            Err(error) => {
                return Err(close_after_physical_failure(
                    operator.as_mut(),
                    error,
                    "execute directional row consumer input",
                ));
            }
        };
        let control = if let Some(batch) = batch {
            if batch.rows.len() != 1 {
                return Err(close_after_physical_failure(
                    operator.as_mut(),
                    crate::ExecError::Other(format!(
                        "directional query operator returned {} rows in one pull",
                        batch.rows.len()
                    )),
                    "execute directional row consumer input",
                ));
            }
            let crate::Batch { schema, mut rows } = batch;
            let row = crate::OwnedPhysicalRow::new(
                schema,
                rows.pop().expect("directional batch width checked"),
            );
            consumer.consume(row)
        } else {
            consumer.direction_exhausted()
        };
        let mut control = match control {
            Ok(control) => control,
            Err(error) => {
                return Err(close_after_physical_failure(
                    operator.as_mut(),
                    crate::ExecError::SQL(error),
                    "consume directional query row",
                ));
            }
        };
        loop {
            match control {
                QueryConsumerControl::Continue => break,
                QueryConsumerControl::Stop => {
                    operator.close().map_err(physical_exec_error)?;
                    return Ok(());
                }
                QueryConsumerControl::Rewind => {
                    if let Err(error) = operator.rewind() {
                        return Err(close_after_physical_failure(
                            operator.as_mut(),
                            error,
                            "rewind directional query input",
                        ));
                    }
                    control = match consumer.rewound() {
                        Ok(control) => control,
                        Err(error) => {
                            return Err(close_after_physical_failure(
                                operator.as_mut(),
                                crate::ExecError::SQL(error),
                                "acknowledge directional query rewind",
                            ));
                        }
                    };
                }
            }
        }
    }
}
