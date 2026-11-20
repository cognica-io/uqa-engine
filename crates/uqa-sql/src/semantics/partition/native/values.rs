//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Check stored carriers against their original key type, without assignment or domain revalidation.

use super::invalid;
use crate::{catalog::type_metadata::pg_type_oid, expr::SQLValueCatalog, ColumnType, SQLError};
use uqa_core::{
    memory::{BudgetedSmallVec, ProductionControl},
    LegacyVectorKind, TemporalValue, Value,
};

pub(super) async fn validate<'v>(
    value: &Value,
    ty: &ColumnType,
    catalog: &impl Fn() -> Option<&'v dyn SQLValueCatalog>,
    control: &ProductionControl<'_>,
    work: &mut (impl crate::catalog::CatalogWork + Send),
) -> Result<(), SQLError> {
    enum Pending<'a, 'c> {
        Value(&'a Value, &'a ColumnType),
        Elements(uqa_core::ControlledArrayElements<'a, 'c>, &'a ColumnType),
    }
    let mut pending = BudgetedSmallVec::<[_; 4]>::new(
        control
            .budget()
            .expect("native partition validation allowance"),
    );
    pending.push(Pending::Value(value, ty))?;
    'next: while let Some(item) = pending.pop() {
        work.checkpoint().await?;
        control.check()?;
        let (value, mut ty) = match item {
            Pending::Value(value, ty) => (value, ty),
            Pending::Elements(mut values, element) => {
                if let Some(value) = values.next_element()? {
                    pending.push(Pending::Elements(values, element))?;
                    pending.push(Pending::Value(value, element))?;
                }
                continue;
            }
        };
        if matches!(value, Value::Null) {
            continue;
        }
        loop {
            work.checkpoint().await?;
            control.check()?;
            // Physical identity inspection keeps retained tuples lazy and never repeats newer domain checks.
            if let Value::Datum(datum) = value {
                if i64::from(datum.type_oid()) == pg_type_oid(ty) {
                    continue 'next;
                }
            }
            if let ColumnType::Domain { base, .. } = ty {
                ty = base;
            } else {
                break;
            }
        }
        let valid = match (ty, value) {
            (ColumnType::Array(_), Value::Array(array)) => {
                let element = ty.array_element_type().expect("array type");
                if array
                    .element_type_oid()
                    .is_some_and(|oid| i64::from(oid) != pg_type_oid(element))
                {
                    false
                } else {
                    pending.push(Pending::Elements(
                        array.elements_with_control(control)?,
                        element,
                    ))?;
                    true
                }
            }
            (ColumnType::Int2Vector, Value::LegacyVector(vector)) => {
                vector.kind() == LegacyVectorKind::SmallInteger
            }
            (ColumnType::OidVector, Value::LegacyVector(vector)) => {
                vector.kind() == LegacyVectorKind::Oid
            }
            (ColumnType::Vector(width), Value::List(values)) => {
                vector(values, *width, control, work).await?
            }
            (ColumnType::Tensor(width), Value::List(values)) => {
                for value in values {
                    work.checkpoint().await?;
                    control.check()?;
                    let Value::List(values) = value else {
                        return Err(invalid("tensor bound carrier"));
                    };
                    if !vector(values, *width, control, work).await? {
                        return Err(invalid("tensor bound width"));
                    }
                }
                true
            }
            _ => scalar(value, ty, catalog())?,
        };
        if !valid {
            return Err(invalid("constant carrier differs from partition key type"));
        }
    }
    Ok(())
}

fn scalar(
    value: &Value,
    ty: &ColumnType,
    catalog: Option<&dyn SQLValueCatalog>,
) -> Result<bool, SQLError> {
    Ok(match (ty, value) {
        (ColumnType::SmallInteger, Value::Int(value)) => i16::try_from(*value).is_ok(),
        (ColumnType::Integer, Value::Int(value)) => i32::try_from(*value).is_ok(),
        (ColumnType::BigInteger, Value::Int(_))
        | (ColumnType::AclItem, Value::Int(_))
        | (ColumnType::Boolean, Value::Bool(_))
        | (ColumnType::Void, Value::Void)
        | (ColumnType::Real | ColumnType::DoublePrecision, Value::Float(_))
        | (ColumnType::Numeric { .. }, Value::Decimal(_))
        | (ColumnType::Bytea, Value::Bytes(_))
        | (ColumnType::Bit(_) | ColumnType::VarBit(_), Value::BitString(_))
        | (ColumnType::Json, Value::Json(_))
        | (ColumnType::JsonB, Value::JsonB(_))
        | (ColumnType::Bpchar | ColumnType::Character(_), Value::FixedChar(_))
        | (
            ColumnType::Text
            | ColumnType::Varchar(_)
            | ColumnType::Name
            | ColumnType::RefCursor
            | ColumnType::Uuid
            | ColumnType::Xml
            | ColumnType::PgNodeTree
            | ColumnType::AclItem
            | ColumnType::InternalChar
            | ColumnType::Range(_)
            | ColumnType::Multirange(_),
            Value::Str(_),
        ) => true,
        (
            ColumnType::Oid
            | ColumnType::Xid
            | ColumnType::Regproc
            | ColumnType::Regprocedure
            | ColumnType::Regclass
            | ColumnType::Regnamespace
            | ColumnType::Regcollation
            | ColumnType::Regrole
            | ColumnType::Regtype,
            Value::Int(value),
        ) => u32::try_from(*value).is_ok(),
        (ty, Value::Temporal(value)) => temporal(ty, value),
        (ColumnType::Enum(reference), Value::Enum(value)) if reference.oid == value.type_oid() => {
            let labels = catalog
                .ok_or_else(|| invalid("missing enum catalog"))?
                .enum_type_labels(reference.oid)?
                .ok_or_else(|| invalid("missing enum labels"))?;
            labels
                .by_key(value.key())
                .is_some_and(|label| value.label_oid().is_none_or(|oid| oid == label.oid))
        }
        (ColumnType::Composite(reference), Value::Record(record)) => {
            record.type_oid() == Some(reference.oid)
        }
        (ColumnType::Record, Value::Record(_) | Value::Row(_)) => true,
        _ => false,
    })
}

fn temporal(ty: &ColumnType, value: &TemporalValue) -> bool {
    matches!(
        (ty, value),
        (ColumnType::Date, TemporalValue::Date { .. })
            | (
                ColumnType::Time | ColumnType::TimePrecision(_),
                TemporalValue::Time { .. },
            )
            | (
                ColumnType::TimeTz | ColumnType::TimeTzPrecision(_),
                TemporalValue::TimeTz { .. },
            )
            | (
                ColumnType::Timestamp | ColumnType::TimestampPrecision(_),
                TemporalValue::Timestamp { .. },
            )
            | (
                ColumnType::TimestampTz | ColumnType::TimestampTzPrecision(_),
                TemporalValue::TimestampTz { .. },
            )
            | (
                ColumnType::Interval | ColumnType::IntervalWithFields { .. },
                TemporalValue::Interval { .. },
            )
    )
}

async fn vector(
    values: &[Value],
    width: u32,
    control: &ProductionControl<'_>,
    work: &mut (impl crate::catalog::CatalogWork + Send),
) -> Result<bool, SQLError> {
    if values.len() != width as usize {
        return Ok(false);
    }
    for value in values {
        work.checkpoint().await?;
        control.check()?;
        if !matches!(value, Value::Float(_)) {
            return Ok(false);
        }
    }
    Ok(true)
}
