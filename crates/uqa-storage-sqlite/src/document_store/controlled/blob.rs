//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Native BLOB payloads decode from borrowed record bytes into charged output buffers.

use super::{typed, Budgeted, BudgetedVec, JsonReadError, StorageReadControl, Value};
use crate::document_store::{
    BLOB_MARKER_ENCODING, BLOB_MARKER_FIELD, BLOB_MARKER_TYPE, BLOB_MARKER_VALUE,
    VALUE_BLOB_F64_LIST, VALUE_BLOB_F64_TENSOR, VALUE_BLOB_MARKER_VALUE, VALUE_BLOB_TYPED_JSON,
};

#[derive(Clone, Copy)]
pub(super) enum Encoding {
    Bytes,
    F64List,
    F64Tensor,
    Typed,
}

#[derive(Clone, Copy)]
pub(in crate::document_store) struct Marker<'a> {
    pub(in crate::document_store) field: &'a str,
    encoding: Encoding,
}

impl Marker<'_> {
    pub(in crate::document_store) fn invalid_reason(self) -> &'static str {
        match self.encoding {
            Encoding::Bytes => "invalid bytes encoding",
            Encoding::F64List => "invalid f64-list encoding",
            Encoding::F64Tensor => "invalid f64-tensor encoding",
            Encoding::Typed => "invalid typed-value encoding",
        }
    }
}

pub(in crate::document_store) fn marker(value: &Value) -> Option<Marker<'_>> {
    let Value::Map(fields) = value else {
        return None;
    };
    let (Some(Value::Str(kind)), Some(Value::Str(field))) =
        (fields.get(BLOB_MARKER_TYPE), fields.get(BLOB_MARKER_FIELD))
    else {
        return None;
    };
    let encoding = if kind == BLOB_MARKER_VALUE {
        Encoding::Bytes
    } else if kind == VALUE_BLOB_MARKER_VALUE {
        match fields.get(BLOB_MARKER_ENCODING) {
            Some(Value::Str(encoding)) if encoding == VALUE_BLOB_F64_LIST => Encoding::F64List,
            Some(Value::Str(encoding)) if encoding == VALUE_BLOB_F64_TENSOR => Encoding::F64Tensor,
            Some(Value::Str(encoding)) if encoding == VALUE_BLOB_TYPED_JSON => Encoding::Typed,
            _ => return None,
        }
    } else {
        return None;
    };
    Some(Marker { field, encoding })
}

pub(in crate::document_store) fn decode_blob(
    bytes: &[u8],
    marker: Marker<'_>,
    control: &StorageReadControl,
) -> Result<Budgeted<Value>, JsonReadError> {
    control.cancellation().check()?;
    match marker.encoding {
        Encoding::Bytes => {
            let mut output = BudgetedVec::new(control.memory());
            output.reserve(bytes.len())?;
            for chunk in bytes.chunks(4096) {
                control.cancellation().check()?;
                output.extend_from_slice(chunk)?;
            }
            let (output, memory) = output.into_parts();
            Ok(Budgeted::new(Value::Bytes(output), memory))
        }
        Encoding::F64List => f64_list(bytes, control),
        Encoding::F64Tensor => f64_tensor(bytes, control),
        Encoding::Typed => typed::decode(bytes, control, 127, false),
    }
}

fn f64_list(bytes: &[u8], control: &StorageReadControl) -> Result<Budgeted<Value>, JsonReadError> {
    if !bytes.len().is_multiple_of(size_of::<f64>()) {
        return Err(JsonReadError::InvalidJson);
    }
    let mut output = BudgetedVec::new(control.memory());
    output.reserve(bytes.len() / size_of::<f64>())?;
    for chunk in bytes.chunks_exact(size_of::<f64>()) {
        control.cancellation().check()?;
        output.push(Value::Float(f64::from_le_bytes(
            chunk.try_into().expect("fixed-width float"),
        )))?;
    }
    let (output, memory) = output.into_parts();
    Ok(Budgeted::new(Value::List(output), memory))
}

fn f64_tensor(
    bytes: &[u8],
    control: &StorageReadControl,
) -> Result<Budgeted<Value>, JsonReadError> {
    if bytes.len() < 8 {
        return Err(JsonReadError::InvalidJson);
    }
    let rows = usize::try_from(u32::from_le_bytes(
        bytes[..4].try_into().expect("row count"),
    ))
    .map_err(|_| JsonReadError::InvalidJson)?;
    let cols = usize::try_from(u32::from_le_bytes(
        bytes[4..8].try_into().expect("column count"),
    ))
    .map_err(|_| JsonReadError::InvalidJson)?;
    let row_len = cols
        .checked_mul(size_of::<f64>())
        .ok_or(JsonReadError::InvalidJson)?;
    let payload_len = rows
        .checked_mul(row_len)
        .ok_or(JsonReadError::InvalidJson)?;
    if rows == 0 || cols == 0 || bytes.len() - 8 != payload_len {
        return Err(JsonReadError::InvalidJson);
    }
    let mut output = BudgetedVec::new(control.memory());
    output.reserve(rows)?;
    let mut memory = control.memory().empty_reservation();
    for row in bytes[8..].chunks_exact(row_len) {
        let (value, retained) = f64_list(row, control)?.into_parts();
        memory.absorb(retained);
        output.push(value)?;
    }
    let (output, retained) = output.into_parts();
    memory.absorb(retained);
    Ok(Budgeted::new(Value::List(output), memory))
}
