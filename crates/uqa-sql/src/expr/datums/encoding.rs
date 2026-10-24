//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Physical SQL value production shares the tuple, array and scalar codecs under the original allowance.

use crate::{
    catalog::type_metadata::{pg_type_len, pg_type_oid},
    expr::composites::{self, CompositeTypeCatalog},
    ColumnType, SQLError,
};
use uqa_core::{
    memory::{Produced, ProductionControl, ProductionVec},
    Value,
};

/// Borrow admitted leaf bytes and keep fixed-width scratch inline. Only representations that actually need construction own a new allowance.
pub(crate) enum EncodedDatum<'a> {
    Borrowed(&'a [u8]),
    Inline { bytes: [u8; 64], length: usize },
    Owned(Produced<Vec<u8>>),
}

impl std::ops::Deref for EncodedDatum<'_> {
    type Target = [u8];
    fn deref(&self) -> &[u8] {
        match self {
            Self::Borrowed(bytes) => bytes,
            Self::Inline { bytes, length } => &bytes[..*length],
            Self::Owned(bytes) => bytes,
        }
    }
}

/// Encode admitted values without invoking declared input, domain checks or canonical support. Native range text keeps its existing parsing adapter. Variable-length results exclude the outer four-byte header owned by the containing field.
pub(crate) fn encode<'a>(
    value: &'a Value,
    mut ty: &ColumnType,
    catalog: Option<&dyn CompositeTypeCatalog>,
    control: &ProductionControl<'_>,
) -> Result<EncodedDatum<'a>, SQLError> {
    control.check()?;
    while let ColumnType::Domain { base, .. } = ty {
        ty = base;
    }
    if let Value::Datum(datum) = value {
        return retained(datum, ty, control);
    }
    let mut inline = [0; 64];
    let length = pg_type_len(ty);
    if matches!(length, 1 | 2 | 4 | 8) {
        let bits = composites::datum::encode_bits(value, ty).ok_or_else(invalid)?;
        inline[..length as usize].copy_from_slice(&bits.to_le_bytes()[..length as usize]);
        return Ok(EncodedDatum::Inline {
            bytes: inline,
            length: length as usize,
        });
    }
    match (value, ty) {
        (Value::Str(text), ColumnType::Name) if text.len() < 64 => {
            inline[..text.len()].copy_from_slice(text.as_bytes());
            Ok(EncodedDatum::Inline {
                bytes: inline,
                length: 64,
            })
        }
        (Value::Str(text), ColumnType::Uuid) => {
            inline[..16].copy_from_slice(&crate::expr::uuid::parse_uuid_bytes(text)?);
            Ok(EncodedDatum::Inline {
                bytes: inline,
                length: 16,
            })
        }
        (Value::Temporal(value), _) if matches!(length, 12 | 16) => {
            let (encoded, length) =
                crate::catalog::node_tree::encode_temporal_datum_inline(value, pg_type_oid(ty))?;
            inline[..length].copy_from_slice(&encoded[..length]);
            Ok(EncodedDatum::Inline {
                bytes: inline,
                length,
            })
        }
        (
            Value::Str(text) | Value::FixedChar(text),
            ColumnType::Text
            | ColumnType::Varchar(_)
            | ColumnType::Character(_)
            | ColumnType::Bpchar
            | ColumnType::RefCursor
            | ColumnType::Xml
            | ColumnType::PgNodeTree,
        )
        | (Value::Json(text), ColumnType::Json) => Ok(EncodedDatum::Borrowed(text.as_bytes())),
        (Value::JsonB(text), ColumnType::JsonB) => {
            crate::expr::json::encode_jsonb_datum_with_control(text, control)
                .map(EncodedDatum::Owned)
        }
        (Value::Bytes(source), ColumnType::Bytea) => Ok(EncodedDatum::Borrowed(source)),
        (Value::BitString(bits), ColumnType::Bit(_) | ColumnType::VarBit(_)) => {
            let mut bytes = ProductionVec::new(*control);
            append(&mut bytes, &bits.bit_len().to_le_bytes())?;
            append(&mut bytes, bits.as_bytes())?;
            Ok(EncodedDatum::Owned(bytes.finish()?))
        }
        (Value::Decimal(number), ColumnType::Numeric { .. }) => {
            crate::catalog::node_tree::encode_numeric_datum_with_control(number, control)
                .map(EncodedDatum::Owned)
        }
        (Value::Str(text), ColumnType::Range(_) | ColumnType::Multirange(_)) => {
            crate::expr::range::datum::encode_with_control(text, ty, control)
                .map(EncodedDatum::Owned)
        }
        (Value::Array(array), ColumnType::Array(element)) => {
            composites::retained::array_payload_with_control(array, element, catalog, control)
                .map(EncodedDatum::Owned)
        }
        (Value::LegacyVector(vector), ColumnType::Int2Vector | ColumnType::OidVector) => {
            legacy_vector(vector, ty, catalog, control).map(EncodedDatum::Owned)
        }
        (Value::Record(fields), ColumnType::Composite(reference)) => {
            let descriptor =
                composites::descriptor(catalog, fields.type_oid().unwrap_or(reference.oid))?;
            let tuple = composites::retained::record_bytes_with_control(
                fields,
                &descriptor,
                catalog,
                control,
            )?;
            let (mut tuple, memory) = tuple.into_parts();
            tuple.drain(..4);
            Ok(EncodedDatum::Owned(control.finish(tuple, memory)?))
        }
        _ => Err(invalid()),
    }
}

fn legacy_vector(
    vector: &uqa_core::LegacyVectorValue,
    ty: &ColumnType,
    catalog: Option<&dyn CompositeTypeCatalog>,
    control: &ProductionControl<'_>,
) -> Result<Produced<Vec<u8>>, SQLError> {
    let element = match (vector.kind(), ty) {
        (uqa_core::LegacyVectorKind::SmallInteger, ColumnType::Int2Vector) => {
            &ColumnType::SmallInteger
        }
        (uqa_core::LegacyVectorKind::Oid, ColumnType::OidVector) => &ColumnType::Oid,
        _ => return Err(invalid()),
    };
    composites::retained::array_payload_with_control(vector.as_array(), element, catalog, control)
}

fn retained<'a>(
    datum: &'a uqa_core::DatumValue,
    ty: &ColumnType,
    control: &ProductionControl<'_>,
) -> Result<EncodedDatum<'a>, SQLError> {
    if pg_type_len(ty) == -1 {
        return Ok(match super::payload(datum, control)? {
            super::Payload::Borrowed(bytes) => EncodedDatum::Borrowed(bytes),
            super::Payload::Owned(bytes) => EncodedDatum::Owned(bytes),
        });
    }
    let length = usize::try_from(pg_type_len(ty)).map_err(|_| invalid())?;
    let start = datum.offset() as usize;
    let end = start.checked_add(length).ok_or_else(invalid)?;
    Ok(EncodedDatum::Borrowed(
        datum.bytes().get(start..end).ok_or_else(invalid)?,
    ))
}

pub(crate) fn append(bytes: &mut ProductionVec<'_, u8>, source: &[u8]) -> Result<(), SQLError> {
    bytes.reserve(source.len())?;
    for byte in source {
        bytes.push_copy(*byte)?;
    }
    Ok(())
}

pub(crate) fn pad_to(bytes: &mut ProductionVec<'_, u8>, length: usize) -> Result<(), SQLError> {
    bytes.reserve(length.saturating_sub(bytes.len()))?;
    while bytes.len() < length {
        bytes.push_copy(0)?;
    }
    Ok(())
}

pub(crate) fn header(length: usize) -> Result<[u8; 4], SQLError> {
    length
        .checked_add(4)
        .and_then(|length| u32::try_from(length).ok())
        .and_then(|length| length.checked_mul(4))
        .map(u32::to_le_bytes)
        .ok_or_else(invalid)
}

pub(crate) fn invalid() -> SQLError {
    SQLError::Routine {
        sqlstate: "XX001".into(),
        message: "invalid typed datum encoding".into(),
    }
}

#[cfg(test)]
mod tests;
