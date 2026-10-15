//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Typed scalar Datums in `PostgreSQL` catalog expression nodes.

pub(super) mod numeric;
pub(super) mod temporal;

use super::{invalid, Field, Node};
use crate::catalog::type_metadata::{
    pg_type_by_value, pg_type_collation_oid, pg_type_len, pg_type_modifier, pg_type_oid,
};
use crate::expr::composites::{
    retained::{capture_type, Descriptors},
    CompositeConstantSource, CompositeTypeCatalog,
};
use crate::{ColumnType, SQLError};
use uqa_core::Value;

/// Unresolved ROW fields retain `PostgreSQL`'s unknown/cstring Datum until a consumer selects a type.
pub(super) fn unknown_constant(value: &Value) -> Result<Node, SQLError> {
    let bytes = match value {
        Value::Null => Field::Null,
        Value::Str(text) => {
            let mut bytes = text.as_bytes().to_vec();
            bytes.push(0);
            Field::Datum {
                length: bytes.len(),
                bytes,
            }
        }
        _ => return Err(invalid("unknown constant is not a string or NULL")),
    };
    Ok(Node::new(
        "CONST",
        [
            ("consttype", 705.into()),
            ("consttypmod", (-1).into()),
            ("constcollid", 0.into()),
            ("constlen", (-2).into()),
            ("constbyval", false.into()),
            ("constisnull", matches!(value, Value::Null).into()),
            ("location", (-1).into()),
            ("constvalue", bytes),
        ],
    ))
}

pub(super) fn unknown_text(node: &Node) -> Result<&str, SQLError> {
    let Field::Datum { length, bytes } = node.field("constvalue")? else {
        return Err(invalid("unknown constant has no Datum"));
    };
    if node.integer("constlen")? != -2
        || node.boolean("constbyval")?
        || *length != bytes.len()
        || bytes.last() != Some(&0)
        || bytes[..bytes.len() - 1].contains(&0)
    {
        return Err(invalid("invalid unknown constant Datum"));
    }
    std::str::from_utf8(&bytes[..bytes.len() - 1])
        .map_err(|_| invalid("invalid UTF-8 in unknown Datum"))
}

pub(super) fn constant(value: &Value, ty: &ColumnType) -> Result<Node, SQLError> {
    constant_with_source(value, ty, None, None)
}

/// Encode an already admitted typed constant, preserving any retained physical source and its catalog descriptors.
pub fn constant_with_source(
    value: &Value,
    ty: &ColumnType,
    source: Option<&CompositeConstantSource>,
    catalog: Option<&dyn CompositeTypeCatalog>,
) -> Result<Node, SQLError> {
    let value = source.map_or(value, |source| &source.value);
    let bytes = if matches!(value, Value::Null) {
        Field::Null
    } else {
        let descriptors = if let Some(source) = source {
            source
                .descriptors
                .iter()
                .map(|descriptor| (descriptor.type_oid, std::sync::Arc::new(descriptor.clone())))
                .collect()
        } else if matches!(value, Value::Datum(_)) {
            Descriptors::new()
        } else {
            let mut descriptors = Descriptors::new();
            capture_type(ty, catalog, &mut descriptors)?;
            descriptors
        };
        datum(value, ty, &descriptors)?
    };
    Ok(Node::new(
        "CONST",
        [
            ("consttype", pg_type_oid(ty).into()),
            ("consttypmod", pg_type_modifier(ty).into()),
            ("constcollid", pg_type_collation_oid(ty).into()),
            ("constlen", pg_type_len(ty).into()),
            ("constbyval", pg_type_by_value(ty).into()),
            ("constisnull", matches!(value, Value::Null).into()),
            ("location", (-1).into()),
            ("constvalue", bytes),
        ],
    ))
}

fn datum(value: &Value, ty: &ColumnType, descriptors: &Descriptors) -> Result<Field, SQLError> {
    let mut representation = ty;
    while let ColumnType::Domain { base, .. } = representation {
        representation = base;
    }
    let mut bytes = match (value, representation) {
        (Value::Bool(value), ColumnType::Boolean) => vec![u8::from(*value)],
        (
            Value::Int(value),
            ColumnType::SmallInteger
            | ColumnType::Integer
            | ColumnType::BigInteger
            | ColumnType::Oid
            | ColumnType::Xid
            | ColumnType::Regclass
            | ColumnType::Regtype
            | ColumnType::Regcollation
            | ColumnType::Regnamespace
            | ColumnType::Regrole
            | ColumnType::Regproc
            | ColumnType::Regprocedure,
        ) => value.to_le_bytes().to_vec(),
        (Value::Float(value), ColumnType::Real) => (*value as f32).to_le_bytes().to_vec(),
        (Value::Float(value), ColumnType::DoublePrecision) => value.to_le_bytes().to_vec(),
        (Value::Enum(value), ColumnType::Enum(_)) => value
            .label_oid()
            .ok_or_else(|| invalid("enum constant has no retained label identity"))?
            .to_le_bytes()
            .to_vec(),
        (Value::Str(_), ColumnType::InternalChar) => {
            vec![crate::expr::internal_char::value_byte(value)
                .ok_or_else(|| SQLError::Internal("invalid internal char carrier".into()))?]
        }
        (Value::Str(value) | Value::FixedChar(value), ColumnType::Name) => {
            let mut bytes = value.as_bytes().to_vec();
            if bytes.len() >= 64 {
                return Err(SQLError::Internal("unclipped name Datum".into()));
            }
            bytes.resize(64, 0);
            bytes
        }
        (
            Value::Str(value) | Value::FixedChar(value),
            ColumnType::Text
            | ColumnType::Varchar(_)
            | ColumnType::Bpchar
            | ColumnType::Character(_)
            | ColumnType::RefCursor,
        ) => varlena(value.as_bytes())?,
        (Value::Bytes(value), ColumnType::Bytea) => varlena(value)?,
        (Value::Str(value), ColumnType::Uuid) => crate::expr::parse_uuid_bytes(value)?.to_vec(),
        (Value::Json(value), ColumnType::Json) => varlena(value.as_bytes())?,
        (Value::JsonB(value), ColumnType::JsonB) => {
            varlena(&crate::expr::encode_jsonb_datum(value)?)?
        }
        (Value::Array(array), ColumnType::Array(element)) => {
            let payload =
                crate::expr::composites::retained::array_payload(array, element, descriptors)
                    .ok_or_else(|| {
                        SQLError::Unsupported(format!(
                            "catalog array Datum encoding for {}",
                            ty.sql_name()
                        ))
                    })?;
            varlena(&payload)?
        }
        (Value::Record(fields), ColumnType::Composite(reference)) => {
            crate::expr::composites::retained::record_bytes(fields, reference.oid, descriptors)
                .ok_or_else(|| invalid("composite constant has no retained tuple layout"))?
        }
        (Value::Datum(_), ColumnType::Array(_) | ColumnType::Composite(_)) => {
            let Value::Datum(owned) = crate::expr::datums::copy_constant_field(value, ty)? else {
                return Err(SQLError::Internal(
                    "physical catalog constant changed carrier".into(),
                ));
            };
            owned.bytes().to_vec()
        }
        (Value::Decimal(value), ColumnType::Numeric { .. }) => varlena(&numeric::encode(value)?)?,
        (Value::Temporal(value), _) => temporal::encode(value, pg_type_oid(representation))?,
        _ => {
            return Err(SQLError::Unsupported(format!(
                "catalog Datum encoding for {}",
                ty.sql_name()
            )))
        }
    };
    let length = if pg_type_by_value(ty) {
        bytes.resize(8, 0);
        usize::try_from(pg_type_len(ty))
            .map_err(|_| SQLError::Internal("invalid by-value Datum length".into()))?
    } else {
        bytes.len()
    };
    Ok(Field::Datum { length, bytes })
}

pub(super) fn scalar_text(node: &Node) -> Result<String, SQLError> {
    let oid = u32::try_from(node.integer("consttype")?)
        .map_err(|_| invalid("invalid scalar constant type"))?;
    let ty = crate::catalog::type_metadata::builtin_scalar_type(oid)
        .ok_or_else(|| invalid("unknown scalar constant type"))?;
    if node.boolean("constbyval")? || node.integer("constlen")? != pg_type_len(ty) {
        return Err(invalid("invalid scalar constant layout"));
    }
    let Field::Datum { length, bytes } = node.field("constvalue")? else {
        return Err(invalid("constant has no Datum"));
    };
    if *ty == ColumnType::Uuid {
        if *length != 16 {
            return Err(invalid("invalid UUID Datum length"));
        }
        let bytes = bytes
            .as_slice()
            .try_into()
            .map_err(|_| invalid("invalid UUID Datum length"))?;
        return Ok(crate::expr::format_uuid(bytes));
    }
    let payload = varlena_payload(*length, bytes)?;
    let control = uqa_core::memory::ProductionControl::uncontrolled();
    match ty {
        ColumnType::Bytea => crate::expr::bytea_to_string_with_control(payload, &control)
            .map(|text| text.into_uncontrolled().expect("ordinary bytea text")),
        ColumnType::Json => std::str::from_utf8(payload)
            .map(str::to_owned)
            .map_err(|_| invalid("invalid JSON Datum encoding")),
        ColumnType::JsonB => crate::expr::decode_jsonb_datum_with_control(payload, &control)
            .map(|text| text.into_uncontrolled().expect("ordinary JSONB text")),
        _ => Err(invalid("unsupported scalar constant type")),
    }
}

pub(super) fn varlena_payload(length: usize, bytes: &[u8]) -> Result<&[u8], SQLError> {
    let header = bytes
        .get(..4)
        .and_then(|bytes| bytes.try_into().ok())
        .map(u32::from_le_bytes)
        .ok_or_else(|| invalid("truncated varlena Datum"))?;
    if bytes.len() != length || header & 3 != 0 || usize::try_from(header >> 2).ok() != Some(length)
    {
        return Err(invalid("invalid varlena Datum length"));
    }
    Ok(&bytes[4..])
}

pub(super) fn catalog_text(
    node: &Node,
    catalog: Option<&dyn crate::expr::SQLValueCatalog>,
) -> Result<Option<String>, SQLError> {
    let oid = u32::try_from(node.integer("consttype")?)
        .map_err(|_| invalid("invalid catalog constant type"))?;
    let ty = if let Some(element) = crate::catalog::type_metadata::builtin_array_element(oid) {
        ColumnType::Array(Box::new(element.clone()))
    } else if let Some(ty) = catalog
        .map(|catalog| catalog.value_type_by_oid(oid))
        .transpose()?
        .flatten()
    {
        ty
    } else {
        return Ok(None);
    };
    if node.boolean("constbyval")? != pg_type_by_value(&ty)
        || node.integer("constlen")? != pg_type_len(&ty)
    {
        return Err(invalid("invalid catalog constant layout"));
    }
    let Field::Datum { length, bytes } = node.field("constvalue")? else {
        return Err(invalid("constant has no Datum"));
    };
    if pg_type_len(&ty) == -1 {
        varlena_payload(*length, bytes)?;
    } else if i64::try_from(*length).ok() != Some(pg_type_len(&ty))
        || (!pg_type_by_value(&ty) && bytes.len() != *length)
    {
        return Err(invalid("invalid catalog constant length"));
    }
    let value = Value::Datum(uqa_core::DatumValue::new(oid, 0, bytes.clone()));
    crate::result::format_postgres_text_with_catalog(&value, &ty, catalog).map(Some)
}

fn varlena(payload: &[u8]) -> Result<Vec<u8>, SQLError> {
    let length = payload
        .len()
        .checked_add(4)
        .and_then(|length| u32::try_from(length).ok())
        .filter(|length| *length <= u32::MAX >> 2)
        .ok_or_else(|| SQLError::Routine {
            sqlstate: "54000".into(),
            message: "catalog Datum is too large".into(),
        })?;
    Ok((length << 2)
        .to_le_bytes()
        .into_iter()
        .chain(payload.iter().copied())
        .collect())
}
