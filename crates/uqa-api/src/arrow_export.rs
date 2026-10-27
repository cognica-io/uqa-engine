//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Descriptor-directed Arrow output, independent of query-builder compatibility columns.

use arrow_array::{ArrayRef, RecordBatch, RecordBatchOptions};
use arrow_schema::{DataType, Field, Schema};
use std::{collections::HashMap, sync::Arc};
use uqa_core::{
    memory::{Produced, ProductionControl},
    Value,
};
use uqa_sql::{catalog::type_metadata, expr::EngineHook, ColumnType, SQLError, SQLResult};

mod columns;

/// Output policy is explicit; unsupported lossless mappings never become text implicitly.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ArrowExportPolicy {
    Lossless,
    SQLText,
}

/// Borrow a positional result or retained batch without copying its values or resolving names.
pub trait ArrowRows {
    fn row_count(&self) -> usize;
    fn column_count(&self) -> usize;
    fn column_name(&self, column: usize) -> &str;
    fn column_type(&self, column: usize) -> Option<&ColumnType>;
    fn value_at(&self, row: usize, column: usize) -> Option<&Value>;
}

impl ArrowRows for SQLResult {
    fn row_count(&self) -> usize {
        self.rows.len()
    }

    fn column_count(&self) -> usize {
        self.columns.len()
    }

    fn column_name(&self, column: usize) -> &str {
        &self.columns[column]
    }

    fn column_type(&self, column: usize) -> Option<&ColumnType> {
        self.column_types.get(column).and_then(Option::as_ref)
    }

    fn value_at(&self, row: usize, column: usize) -> Option<&Value> {
        self.value_at(row, column)
    }
}

fn unsupported(message: &str) -> SQLError {
    SQLError::Unsupported(message.into())
}

fn overflow() -> SQLError {
    SQLError::Routine {
        sqlstate: "54000".into(),
        message: "Arrow export exceeds addressable memory".into(),
    }
}

fn arrow_error(error: &arrow_schema::ArrowError) -> SQLError {
    SQLError::Internal(format!("construct Arrow result: {error}"))
}

fn data_type(ty: Option<&ColumnType>, policy: ArrowExportPolicy) -> Result<DataType, SQLError> {
    if policy == ArrowExportPolicy::SQLText {
        return Ok(DataType::LargeUtf8);
    }
    Ok(match ty {
        Some(ColumnType::Boolean) => DataType::Boolean,
        Some(ColumnType::SmallInteger) => DataType::Int16,
        Some(ColumnType::Integer) => DataType::Int32,
        Some(ColumnType::BigInteger) => DataType::Int64,
        Some(ColumnType::Real) => DataType::Float32,
        Some(ColumnType::DoublePrecision) => DataType::Float64,
        Some(
            ColumnType::Text
            | ColumnType::Bpchar
            | ColumnType::Character(_)
            | ColumnType::Varchar(_)
            | ColumnType::Json
            | ColumnType::JsonB,
        ) => DataType::LargeUtf8,
        Some(ColumnType::Bytea) => DataType::LargeBinary,
        Some(ColumnType::Uuid) => DataType::FixedSizeBinary(16),
        Some(ColumnType::Numeric {
            precision: Some(precision),
            scale,
        }) => {
            let precision = u8::try_from(*precision)
                .map_err(|_| unsupported("NUMERIC precision has no lossless Arrow mapping"))?;
            let scale = i8::try_from(scale.unwrap_or(0))
                .map_err(|_| unsupported("NUMERIC scale has no lossless Arrow mapping"))?;
            arrow_array::types::validate_decimal_precision_and_scale::<
                arrow_array::types::Decimal128Type,
            >(precision, scale)
            .map_err(|_| unsupported("NUMERIC modifier has no lossless Arrow decimal mapping"))?;
            DataType::Decimal128(precision, scale)
        }
        _ => {
            return Err(unsupported(
                "SQL type has no qualified lossless Arrow mapping",
            ))
        }
    })
}

fn field(name: &str, ty: Option<&ColumnType>, output: DataType) -> Field {
    let mut metadata = HashMap::with_capacity(4);
    metadata.insert(
        "uqa.sql.type_oid".into(),
        ty.map_or(0, type_metadata::pg_type_oid).to_string(),
    );
    metadata.insert(
        "uqa.sql.type_name".into(),
        ty.map_or("unknown", type_metadata::column_type_name).into(),
    );
    metadata.insert(
        "uqa.sql.type_modifier".into(),
        ty.map_or(-1, type_metadata::pg_type_modifier).to_string(),
    );
    let schema = match ty {
        Some(ColumnType::Domain { schema, .. }) => Some(schema.as_str()),
        Some(ColumnType::Enum(reference)) => Some(reference.schema.as_str()),
        Some(ColumnType::Composite(reference)) => Some(reference.schema.as_str()),
        Some(ColumnType::DeclaredRange(reference) | ColumnType::DeclaredMultirange(reference)) => {
            Some(reference.schema.as_str())
        }
        _ => None,
    };
    if let Some(schema) = schema {
        metadata.insert("uqa.sql.type_schema".into(), schema.into());
    }
    Field::new(name, output, true).with_metadata(metadata)
}

/// Convert under the caller's allowance and both cancellation tokens. The returned
/// lease owns the buffers independently of the original result and session.
pub fn export_arrow(
    source: &dyn ArrowRows,
    policy: ArrowExportPolicy,
    engine: Option<&dyn EngineHook>,
    control: &ProductionControl<'_>,
) -> Result<Produced<RecordBatch>, SQLError> {
    control.check()?;
    // Arrow owns its allocation strategy. Admit the complete fixed-width buffers,
    // validity/offset padding and metadata before invoking its constructors. Text
    // and binary payload capacity is separately admitted after controlled formatting.
    let row_bytes = source.row_count().checked_mul(24).ok_or_else(overflow)?;
    let mut fixed = 1024usize;
    for column in 0..source.column_count() {
        control.check()?;
        data_type(source.column_type(column), policy)?;
        let ty = source.column_type(column);
        let type_name = ty.map_or("unknown", type_metadata::column_type_name);
        // A field holds four metadata entries, an Arc, strings, an array owner and
        // at most three buffers, each rounded to Arrow's 64-byte alignment.
        let metadata = source
            .column_name(column)
            .len()
            .checked_add(type_name.len())
            .and_then(|n| n.checked_mul(4))
            .and_then(|n| n.checked_add(4096))
            .ok_or_else(overflow)?;
        fixed = fixed
            .checked_add(metadata)
            .and_then(|n| n.checked_add(row_bytes))
            .ok_or_else(overflow)?;
    }
    let mut memory = control.reserve(fixed)?;
    let mut fields = Vec::with_capacity(source.column_count());
    let mut arrays: Vec<ArrayRef> = Vec::with_capacity(source.column_count());
    for column in 0..source.column_count() {
        control.check()?;
        let ty = source.column_type(column);
        let output = data_type(ty, policy)?;
        let array = columns::build(source, column, &output, policy, engine, control)?;
        let (array, payload) = array.into_parts();
        memory = control.combine(memory, payload);
        arrays.push(array);
        fields.push(field(source.column_name(column), ty, output));
    }
    let batch = RecordBatch::try_new_with_options(
        Arc::new(Schema::new(fields)),
        arrays,
        &RecordBatchOptions::new().with_row_count(Some(source.row_count())),
    )
    .map_err(|error| arrow_error(&error))?;
    Ok(control.finish(batch, memory)?)
}
