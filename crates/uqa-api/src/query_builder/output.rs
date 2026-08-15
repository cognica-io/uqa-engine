//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[derive(Debug)]
pub enum QueryBuilderError {
    Sql(SQLError),
    Arrow(ArrowError),
    Parquet(ParquetError),
    Io(std::io::Error),
}

impl fmt::Display for QueryBuilderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Sql(err) => write!(f, "{err}"),
            Self::Arrow(err) => write!(f, "{err}"),
            Self::Parquet(err) => write!(f, "{err}"),
            Self::Io(err) => write!(f, "{err}"),
        }
    }
}

impl std::error::Error for QueryBuilderError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Sql(err) => Some(err),
            Self::Arrow(err) => Some(err),
            Self::Parquet(err) => Some(err),
            Self::Io(err) => Some(err),
        }
    }
}

impl From<SQLError> for QueryBuilderError {
    fn from(value: SQLError) -> Self {
        Self::Sql(value)
    }
}

impl QueryBuilder<'_> {
    /// Execute the query and convert the result to an Arrow
    /// [`RecordBatch`]. The batch always includes compatibility
    /// metadata columns `_doc_id` and `_score` before the requested
    /// projections.
    pub fn execute_arrow(&self) -> Result<RecordBatch, QueryBuilderError> {
        let result = self.execute_with_result_metadata()?;
        sql_result_to_record_batch(&result).map_err(QueryBuilderError::Arrow)
    }

    /// Execute the query and write the Arrow result to a Parquet file.
    pub fn execute_parquet<P: AsRef<Path>>(&self, path: P) -> Result<(), QueryBuilderError> {
        let batch = self.execute_arrow()?;
        let file = File::create(path).map_err(QueryBuilderError::Io)?;
        let mut writer =
            ArrowWriter::try_new(file, batch.schema(), None).map_err(QueryBuilderError::Parquet)?;
        writer.write(&batch).map_err(QueryBuilderError::Parquet)?;
        writer.close().map_err(QueryBuilderError::Parquet)?;
        Ok(())
    }

    fn execute_with_result_metadata(&self) -> Result<SQLResult, SQLError> {
        let mut builder = self.clone();
        let original = if builder.projections.is_empty() {
            vec!["*".to_string()]
        } else {
            builder.projections
        };
        builder.projections = Vec::with_capacity(original.len() + 2);
        push_projection_once(&mut builder.projections, "_doc_id");
        push_projection_once(&mut builder.projections, "_score");
        for projection in original {
            if projection != "_doc_id" && projection != "_score" {
                builder.projections.push(projection);
            }
        }
        builder.execute()
    }
}

fn push_projection_once(projections: &mut Vec<String>, projection: &str) {
    if !projections.iter().any(|p| p == projection) {
        projections.push(projection.to_string());
    }
}

pub(super) fn sql_result_to_record_batch(result: &SQLResult) -> Result<RecordBatch, ArrowError> {
    let fields: Vec<Field> = result
        .columns
        .iter()
        .map(|column| Field::new(column, infer_arrow_type(column, result), true))
        .collect();
    let arrays: Vec<ArrayRef> = result
        .columns
        .iter()
        .zip(fields.iter())
        .map(|(column, field)| build_arrow_array(column, field.data_type(), result))
        .collect::<Result<_, _>>()?;
    RecordBatch::try_new(Arc::new(Schema::new(fields)), arrays)
}

pub(super) fn infer_arrow_type(column: &str, result: &SQLResult) -> DataType {
    if column == "_doc_id" {
        return DataType::Int64;
    }
    if column == "_score" {
        return DataType::Float64;
    }

    let mut ty: Option<DataType> = None;
    for row in &result.rows {
        let Some(value) = row.get(column) else {
            continue;
        };
        let next = match value {
            Value::Null => continue,
            Value::Bool(_) => DataType::Boolean,
            Value::Int(_) => DataType::Int64,
            Value::Float(_) => DataType::Float64,
            Value::Decimal(_)
            | Value::Str(_)
            | Value::FixedChar(_)
            | Value::Json(_)
            | Value::JsonB(_)
            | Value::Bytes(_)
            | Value::Temporal(_)
            | Value::Array(_)
            | Value::List(_)
            | Value::Row(_)
            | Value::Record(_)
            | Value::Map(_) => DataType::Utf8,
        };
        ty = Some(match (ty, next) {
            (None, dt) => dt,
            (Some(DataType::Int64), DataType::Float64)
            | (Some(DataType::Float64), DataType::Int64) => {
                if column_integers_fit_f64(column, result) {
                    DataType::Float64
                } else {
                    DataType::Utf8
                }
            }
            (Some(DataType::Float64), DataType::Float64) => DataType::Float64,
            (Some(current), dt) if current == dt => current,
            _ => DataType::Utf8,
        });
        if ty == Some(DataType::Utf8) {
            break;
        }
    }
    ty.unwrap_or(DataType::Utf8)
}

fn build_arrow_array(
    column: &str,
    data_type: &DataType,
    result: &SQLResult,
) -> Result<ArrayRef, ArrowError> {
    let array: ArrayRef = match data_type {
        DataType::Boolean => Arc::new(BooleanArray::from(collect_typed_column(
            column,
            result,
            |value| match value {
                Value::Bool(value) => Some(*value),
                _ => None,
            },
            "boolean",
        )?)),
        DataType::Int64 => Arc::new(Int64Array::from(collect_typed_column(
            column,
            result,
            |value| match value {
                Value::Int(value) => Some(*value),
                _ => None,
            },
            "int64",
        )?)),
        DataType::Float64 => Arc::new(Float64Array::from(collect_float_column(column, result)?)),
        _ => Arc::new(StringArray::from(
            result
                .rows
                .iter()
                .map(|row| row.get(column).and_then(value_to_arrow_string))
                .collect::<Vec<_>>(),
        )),
    };
    Ok(array)
}

fn collect_typed_column<T, F>(
    column: &str,
    result: &SQLResult,
    convert: F,
    expected: &str,
) -> Result<Vec<Option<T>>, ArrowError>
where
    F: Fn(&Value) -> Option<T>,
{
    result
        .rows
        .iter()
        .map(|row| match row.get(column) {
            None | Some(Value::Null) => Ok(None),
            Some(value) => convert(value).map(Some).ok_or_else(|| {
                ArrowError::CastError(format!(
                    "column `{column}` contains {} where {expected} was inferred",
                    value_kind(value)
                ))
            }),
        })
        .collect()
}

fn collect_float_column(column: &str, result: &SQLResult) -> Result<Vec<Option<f64>>, ArrowError> {
    result
        .rows
        .iter()
        .map(|row| match row.get(column) {
            None | Some(Value::Null) => Ok(None),
            Some(Value::Float(value)) => Ok(Some(*value)),
            Some(Value::Int(value)) => i64_to_f64_exact(*value).map(Some).ok_or_else(|| {
                ArrowError::CastError(format!(
                    "column `{column}` integer {value} cannot be represented exactly as float64"
                ))
            }),
            Some(value) => Err(ArrowError::CastError(format!(
                "column `{column}` contains {} where float64 was inferred",
                value_kind(value)
            ))),
        })
        .collect()
}

fn column_integers_fit_f64(column: &str, result: &SQLResult) -> bool {
    result.rows.iter().all(|row| {
        !matches!(
            row.get(column),
            Some(Value::Int(value)) if i64_to_f64_exact(*value).is_none()
        )
    })
}

fn i64_to_f64_exact(value: i64) -> Option<f64> {
    const MAX_SAFE_INTEGER: i64 = 1_i64 << 53;
    (-MAX_SAFE_INTEGER..=MAX_SAFE_INTEGER)
        .contains(&value)
        .then_some(value as f64)
}

fn value_kind(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Int(_) => "integer",
        Value::Float(_) => "float",
        Value::Decimal(_) => "decimal",
        Value::Str(_) | Value::FixedChar(_) => "string",
        Value::Json(_) => "json",
        Value::JsonB(_) => "jsonb",
        Value::Bytes(_) => "bytes",
        Value::Temporal(_) => "temporal",
        Value::Array(_) => "array",
        Value::List(_) => "list",
        Value::Row(_) => "row",
        Value::Record(_) => "record",
        Value::Map(_) => "map",
    }
}

fn value_to_arrow_string(value: &Value) -> Option<String> {
    match value {
        Value::Null => None,
        Value::Bool(v) => Some(v.to_string()),
        Value::Int(v) => Some(v.to_string()),
        Value::Float(v) => Some(v.to_string()),
        Value::Decimal(v) => Some(v.to_sql_string()),
        Value::Str(v) | Value::FixedChar(v) | Value::Json(v) | Value::JsonB(v) => Some(v.clone()),
        Value::Bytes(v) => Some(format!("{v:?}")),
        Value::Temporal(v) => Some(v.to_sql_string()),
        Value::Array(v) => Some(uqa_sql::expr::array_value_to_string(v)),
        Value::List(v) => Some(format!("{v:?}")),
        Value::Row(_) | Value::Record(_) => Some(uqa_sql::expr::value_to_string(value)),
        Value::Map(v) => Some(format!("{v:?}")),
    }
}
