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
use crate::{ColumnType, SQLError};
use uqa_core::Value;

pub(super) fn constant(value: &Value, ty: &ColumnType) -> Result<Node, SQLError> {
    let bytes = if matches!(value, Value::Null) {
        Field::Null
    } else {
        datum(value, ty)?
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

fn datum(value: &Value, ty: &ColumnType) -> Result<Field, SQLError> {
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
            let payload = crate::expr::composites::retained::array_payload(
                array,
                element,
                &crate::expr::composites::retained::Descriptors::new(),
            )
            .ok_or_else(|| {
                SQLError::Unsupported(format!(
                    "catalog array Datum encoding for {}",
                    ty.sql_name()
                ))
            })?;
            varlena(&payload)?
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

pub(super) fn array_text(node: &Node) -> Result<String, SQLError> {
    let oid = u32::try_from(node.integer("consttype")?)
        .map_err(|_| invalid("invalid array constant type"))?;
    let element = crate::catalog::type_metadata::builtin_array_element(oid)
        .ok_or_else(|| invalid("unknown array constant type"))?;
    if node.boolean("constbyval")? || node.integer("constlen")? != -1 {
        return Err(invalid("invalid array constant layout"));
    }
    let Field::Datum { length, bytes } = node.field("constvalue")? else {
        return Err(invalid("constant has no Datum"));
    };
    varlena_payload(*length, bytes)?;
    let value = crate::expr::datums::read(&uqa_core::DatumValue::new(oid, 0, bytes.clone()))?;
    crate::result::format_postgres_text(&value, &ColumnType::Array(Box::new(element.clone())), None)
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
