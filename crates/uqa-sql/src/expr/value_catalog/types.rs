//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Resolve retained field identities without repeating input or modifier coercion.

use super::SQLValueCatalog;
use crate::{
    ast::IntervalFields,
    catalog::type_metadata::{builtin_array_element, builtin_scalar_type},
    ColumnType, SQLError,
};
use uqa_core::memory::{Produced, ProductionControl};

pub(crate) fn field_type_with_control(
    oid: u32,
    modifier: i32,
    catalog: Option<&dyn SQLValueCatalog>,
    control: &ProductionControl<'_>,
) -> Result<Produced<ColumnType>, SQLError> {
    let ty = type_by_oid_with_control(oid, catalog, control)?;
    let (mut ty, memory) = ty.into_parts();
    let mut base = &mut ty;
    while let ColumnType::Array(element) = base {
        base = element;
    }
    if modifier >= 0 {
        *base = match base {
            ColumnType::Varchar(_) if modifier >= 4 => {
                ColumnType::Varchar(Some((modifier - 4) as u32))
            }
            ColumnType::Bpchar | ColumnType::Character(_) if modifier >= 4 => {
                ColumnType::Character((modifier - 4) as u32)
            }
            ColumnType::Numeric { .. } if modifier >= 4 => ColumnType::Numeric {
                precision: Some(((modifier - 4) >> 16) as u32),
                scale: Some((((modifier - 4) & 0x7ff) ^ 1024) - 1024),
            },
            ColumnType::Bit(_) => ColumnType::Bit(Some(modifier as u32)),
            ColumnType::VarBit(_) => ColumnType::VarBit(Some(modifier as u32)),
            ColumnType::Time | ColumnType::TimePrecision(_) => {
                ColumnType::TimePrecision(modifier as u32)
            }
            ColumnType::TimeTz | ColumnType::TimeTzPrecision(_) => {
                ColumnType::TimeTzPrecision(modifier as u32)
            }
            ColumnType::Timestamp | ColumnType::TimestampPrecision(_) => {
                ColumnType::TimestampPrecision(modifier as u32)
            }
            ColumnType::TimestampTz | ColumnType::TimestampTzPrecision(_) => {
                ColumnType::TimestampTzPrecision(modifier as u32)
            }
            ColumnType::Interval | ColumnType::IntervalWithFields { .. } => {
                let fields = IntervalFields::from_modifier_mask(i64::from(modifier >> 16))
                    .ok_or_else(|| {
                        SQLError::Internal("invalid retained interval modifier".into())
                    })?;
                let precision = i64::from(modifier & 65535);
                ColumnType::with_interval_modifiers(
                    fields,
                    (precision != 65535).then_some(precision),
                )?
            }
            _ => return Ok(control.finish(ty, memory)?),
        };
    }
    Ok(control.finish(ty, memory)?)
}

pub(crate) fn type_by_oid_with_control(
    oid: u32,
    catalog: Option<&dyn SQLValueCatalog>,
    control: &ProductionControl<'_>,
) -> Result<Produced<ColumnType>, SQLError> {
    control.check()?;
    if let Some(ty) = builtin_scalar_type(oid) {
        return Ok(ty.clone_with_control(control)?);
    }
    if let Some(element) = builtin_array_element(oid) {
        return Ok(ColumnType::array_with_control(
            element.clone_with_control(control)?,
            control,
        )?);
    }
    if let Some(ty) = catalog
        .map(|catalog| catalog.value_type_by_oid_with_control(oid, control))
        .transpose()?
        .flatten()
    {
        return Ok(ty);
    }
    let (message, _memory) = control
        .format(format_args!("cache lookup failed for type {oid}"))?
        .into_parts();
    Err(SQLError::Routine {
        sqlstate: "XX000".into(),
        message,
    })
}
