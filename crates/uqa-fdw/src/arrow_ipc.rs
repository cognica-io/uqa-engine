//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Arrow IPC foreign data wrapper.
//!
//! `ArrowIpcHandler` reads Arrow IPC file or stream sources declared
//! through `CREATE FOREIGN TABLE ... OPTIONS (source '...')`, applies
//! predicate pushdown in the FDW layer, then projects and limits rows
//! before returning standard UQA row maps.

use std::fs::File;
use std::io::BufReader;

use arrow_array::{
    Array, BinaryArray, BooleanArray, Date32Array, Date64Array, Float32Array, Float64Array,
    Int16Array, Int32Array, Int64Array, Int8Array, LargeBinaryArray, LargeStringArray, RecordBatch,
    StringArray, Time64MicrosecondArray, Time64NanosecondArray, TimestampMicrosecondArray,
    TimestampMillisecondArray, TimestampNanosecondArray, TimestampSecondArray, UInt16Array,
    UInt32Array, UInt64Array, UInt8Array,
};
use arrow_ipc::reader::{FileReader, StreamReader};
use arrow_schema::{DataType, TimeUnit};
use uqa_core::{TemporalValue, Value};

use crate::{
    project_row, row_matches_predicates, FDWError, FDWHandler, FDWPredicate, ForeignServer,
    ForeignTable, Row,
};

#[derive(Debug, Clone)]
pub struct ArrowIpcHandler {
    server: ForeignServer,
}

impl ArrowIpcHandler {
    pub fn new(server: ForeignServer) -> Self {
        Self { server }
    }
}

impl FDWHandler for ArrowIpcHandler {
    fn scan(
        &self,
        table: &ForeignTable,
        columns: Option<&[String]>,
        predicates: &[FDWPredicate],
        limit: Option<u64>,
    ) -> Result<Vec<Row>, FDWError> {
        let _ = &self.server;
        let source = table
            .options
            .get("source")
            .ok_or_else(|| ArrowIpcPrepareError::MissingSource(table.name.clone()))?;
        let format = table.options.get("format").map_or("file", String::as_str);
        let file = File::open(source)?;
        let mut out = Vec::new();
        match format {
            "file" | "ipc" | "arrow" => {
                let reader = FileReader::try_new(BufReader::new(file), None)?;
                for batch in reader {
                    append_batch_rows(&batch?, table, columns, predicates, limit, &mut out)?;
                    if limit.is_some_and(|cap| out.len() as u64 >= cap) {
                        break;
                    }
                }
            }
            "stream" | "ipc_stream" | "arrow_stream" => {
                let reader = StreamReader::try_new(BufReader::new(file), None)?;
                for batch in reader {
                    append_batch_rows(&batch?, table, columns, predicates, limit, &mut out)?;
                    if limit.is_some_and(|cap| out.len() as u64 >= cap) {
                        break;
                    }
                }
            }
            other => return Err(ArrowIpcPrepareError::UnsupportedFormat(other.into()).into()),
        }
        Ok(out)
    }
}

fn append_batch_rows(
    batch: &RecordBatch,
    table: &ForeignTable,
    columns: Option<&[String]>,
    predicates: &[FDWPredicate],
    limit: Option<u64>,
    out: &mut Vec<Row>,
) -> Result<(), FDWError> {
    let source_columns = source_columns(table, batch);
    let mut indexed = Vec::with_capacity(source_columns.len());
    for name in source_columns {
        let idx = batch
            .schema()
            .index_of(&name)
            .map_err(|_| FDWError::Other(format!("Arrow IPC source missing column `{name}`")))?;
        indexed.push((name, idx));
    }

    for row_idx in 0..batch.num_rows() {
        if let Some(cap) = limit {
            if out.len() as u64 >= cap {
                break;
            }
        }
        let mut row = Row::new();
        for (name, col_idx) in &indexed {
            row.insert(
                name.clone(),
                arrow_value(batch.column(*col_idx).as_ref(), row_idx)?,
            );
        }
        if row_matches_predicates(&row, predicates) {
            out.push(project_row(&row, columns));
        }
    }
    Ok(())
}

fn source_columns(table: &ForeignTable, batch: &RecordBatch) -> Vec<String> {
    if table.columns.is_empty() {
        batch
            .schema()
            .fields()
            .iter()
            .map(|field| field.name().clone())
            .collect()
    } else {
        table.columns.iter().map(|col| col.name.clone()).collect()
    }
}

fn arrow_value(array: &dyn Array, row: usize) -> Result<Value, FDWError> {
    if array.is_null(row) {
        return Ok(Value::Null);
    }
    macro_rules! downcast_value {
        ($ty:ty) => {{
            array
                .as_any()
                .downcast_ref::<$ty>()
                .ok_or_else(|| {
                    FDWError::UnsupportedValue(format!(
                        "Arrow array downcast failed for {:?}",
                        array.data_type()
                    ))
                })?
                .value(row)
        }};
    }

    Ok(match array.data_type() {
        DataType::Boolean => Value::Bool(downcast_value!(BooleanArray)),
        DataType::Int8 => Value::Int(i64::from(downcast_value!(Int8Array))),
        DataType::Int16 => Value::Int(i64::from(downcast_value!(Int16Array))),
        DataType::Int32 => Value::Int(i64::from(downcast_value!(Int32Array))),
        DataType::Int64 => Value::Int(downcast_value!(Int64Array)),
        DataType::UInt8 => Value::Int(i64::from(downcast_value!(UInt8Array))),
        DataType::UInt16 => Value::Int(i64::from(downcast_value!(UInt16Array))),
        DataType::UInt32 => Value::Int(i64::from(downcast_value!(UInt32Array))),
        DataType::UInt64 => {
            let v = downcast_value!(UInt64Array);
            i64::try_from(v).map_or(Value::Str(v.to_string()), Value::Int)
        }
        DataType::Float32 => Value::Float(f64::from(downcast_value!(Float32Array))),
        DataType::Float64 => Value::Float(downcast_value!(Float64Array)),
        DataType::Utf8 => Value::Str(downcast_value!(StringArray).to_string()),
        DataType::LargeUtf8 => Value::Str(downcast_value!(LargeStringArray).to_string()),
        DataType::Binary => Value::Bytes(downcast_value!(BinaryArray).to_vec()),
        DataType::LargeBinary => Value::Bytes(downcast_value!(LargeBinaryArray).to_vec()),
        DataType::Date32 => Value::Temporal(TemporalValue::Date {
            days: downcast_value!(Date32Array),
        }),
        DataType::Date64 => Value::Temporal(TemporalValue::Timestamp {
            micros: downcast_value!(Date64Array) * 1_000,
        }),
        DataType::Time64(TimeUnit::Microsecond) => Value::Temporal(TemporalValue::Time {
            micros: downcast_value!(Time64MicrosecondArray),
        }),
        DataType::Time64(TimeUnit::Nanosecond) => Value::Temporal(TemporalValue::Time {
            micros: downcast_value!(Time64NanosecondArray) / 1_000,
        }),
        DataType::Timestamp(TimeUnit::Second, _) => Value::Temporal(TemporalValue::Timestamp {
            micros: downcast_value!(TimestampSecondArray) * 1_000_000,
        }),
        DataType::Timestamp(TimeUnit::Millisecond, _) => {
            Value::Temporal(TemporalValue::Timestamp {
                micros: downcast_value!(TimestampMillisecondArray) * 1_000,
            })
        }
        DataType::Timestamp(TimeUnit::Microsecond, _) => {
            Value::Temporal(TemporalValue::Timestamp {
                micros: downcast_value!(TimestampMicrosecondArray),
            })
        }
        DataType::Timestamp(TimeUnit::Nanosecond, _) => Value::Temporal(TemporalValue::Timestamp {
            micros: downcast_value!(TimestampNanosecondArray) / 1_000,
        }),
        other => {
            return Err(FDWError::UnsupportedValue(format!(
                "Arrow IPC type {other:?}"
            )));
        }
    })
}

#[derive(Debug, thiserror::Error)]
pub enum ArrowIpcPrepareError {
    #[error("Foreign table `{0}` missing required option `source`")]
    MissingSource(String),
    #[error("Unsupported Arrow IPC format `{0}`")]
    UnsupportedFormat(String),
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::sync::Arc;

    use arrow_array::{Float64Array, Int64Array};
    use arrow_ipc::writer::FileWriter;
    use arrow_schema::{Field, Schema};

    use super::*;
    use crate::{ColumnDef, ColumnType, PredicateOp};

    fn table(source: &str) -> ForeignTable {
        let mut options = BTreeMap::new();
        options.insert("source".into(), source.into());
        ForeignTable {
            name: "books".into(),
            server_name: "arrow".into(),
            columns: vec![
                ColumnDef {
                    name: "id".into(),
                    ty: ColumnType::Integer,
                },
                ColumnDef {
                    name: "title".into(),
                    ty: ColumnType::Text,
                },
                ColumnDef {
                    name: "score".into(),
                    ty: ColumnType::Real,
                },
            ],
            options,
        }
    }

    #[test]
    fn arrow_ipc_handler_scans_file_with_filter_projection_and_limit() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("books.arrow");
        let schema = Arc::new(Schema::new(vec![
            Field::new("id", DataType::Int64, false),
            Field::new("title", DataType::Utf8, false),
            Field::new("score", DataType::Float64, false),
        ]));
        let batch = RecordBatch::try_new(
            Arc::clone(&schema),
            vec![
                Arc::new(Int64Array::from(vec![1, 2, 3])),
                Arc::new(StringArray::from(vec!["Rust", "Python", "UQA"])),
                Arc::new(Float64Array::from(vec![0.9, 0.5, 0.8])),
            ],
        )
        .unwrap();
        {
            let file = File::create(&path).unwrap();
            let mut writer = FileWriter::try_new(file, &schema).unwrap();
            writer.write(&batch).unwrap();
            writer.finish().unwrap();
        }

        let server = ForeignServer {
            name: "arrow".into(),
            fdw_type: "arrow_fdw".into(),
            options: BTreeMap::new(),
        };
        let handler = ArrowIpcHandler::new(server);
        let cols = ["title".to_string()];
        let rows = handler
            .scan(
                &table(&path.to_string_lossy()),
                Some(&cols),
                &[FDWPredicate {
                    column: "score".into(),
                    operator: PredicateOp::Gt,
                    value: Value::Float(0.7),
                }],
                Some(1),
            )
            .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].get("title"), Some(&Value::Str("Rust".into())));
        assert!(!rows[0].contains_key("score"));
    }
}
