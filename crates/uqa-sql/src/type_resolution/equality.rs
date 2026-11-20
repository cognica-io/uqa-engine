//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `PostgreSQL` equality-operator operand binding.

use crate::ast::{ColumnType, ColumnTypeInputWork};
use crate::catalog::work::{inline, ReferenceWork};
use crate::SQLError;
use uqa_core::memory::{Produced, ProductionControl};

use super::{
    common::{
        base_type, base_type_with_work, common_type_with_control, same_operator_type_with_work,
    },
    operators::equality_operator_available_with_work,
    overload_resolution::{canonical_column_type_name_with_work, implicit_cast_with_work},
};

pub fn equality_operand_type(
    left: &ColumnType,
    right: &ColumnType,
) -> Result<ColumnType, SQLError> {
    equality_operand_type_with_control(left, right, &ProductionControl::uncontrolled()).map(|ty| {
        ty.into_uncontrolled()
            .expect("ordinary equality type has no reservation")
    })
}

/// Preserve the existing equality coercion rules while retaining any copied type payload under the caller's allowance.
pub fn equality_operand_type_with_control(
    left: &ColumnType,
    right: &ColumnType,
    control: &ProductionControl<'_>,
) -> Result<Produced<ColumnType>, SQLError> {
    inline(equality_with_work(
        left,
        right,
        control,
        &mut ReferenceWork {
            control,
            work: &mut (),
        },
    ))?
    .ok_or_else(|| undefined_equality_operator(left, right))
}

async fn equality_with_work<W: ColumnTypeInputWork<Error = SQLError>>(
    left: &ColumnType,
    right: &ColumnType,
    control: &ProductionControl<'_>,
    work: &mut W,
) -> Result<Option<Produced<ColumnType>>, SQLError> {
    control.check()?;
    if !equality_operator_available_with_work(left, control, work).await?
        || !equality_operator_available_with_work(right, control, work).await?
    {
        return Ok(None);
    }
    if matches!(left, ColumnType::Array(_)) || matches!(right, ColumnType::Array(_)) {
        if same_operator_type_with_work(left, right, control, work).await? {
            return left.clone_with_input_work(control, work).await.map(Some);
        }
        return Ok(None);
    }
    if left.is_character_string() && right.is_character_string() {
        let comparison = if matches!(left, ColumnType::Bpchar | ColumnType::Character(_))
            || matches!(right, ColumnType::Bpchar | ColumnType::Character(_))
        {
            ColumnType::Bpchar
        } else {
            ColumnType::Text
        };
        return Ok(Some(
            control.finish(comparison, control.empty_reservation())?,
        ));
    }
    match common_type_with_control(left, right, control) {
        Ok(value) => Ok(Some(value)),
        Err(error) if matches!(error.sqlstate(), Some("53200" | "57014")) => Err(error),
        Err(_) => Ok(None),
    }
}

/// Resolve the comparison type accepted by a `PostgreSQL` foreign key. The referenced key's operator class makes compatibility directional: an implicit cast from the referencing type to the referenced type is valid, while the reverse cast alone is not. Integer widths, floating widths, character strings, and date/timestamp types additionally have cross-type equality operators in their shared operator families.
pub fn foreign_key_operand_type(
    referencing: &ColumnType,
    referenced: &ColumnType,
) -> Result<ColumnType, SQLError> {
    foreign_key_operand_type_with_control(
        referencing,
        referenced,
        &ProductionControl::uncontrolled(),
    )
    .map(|ty| {
        ty.into_uncontrolled()
            .expect("ordinary foreign key type has no reservation")
    })
}

/// Retain comparison types and canonical type names under the caller's allowance while applying the ordinary foreign-key coercion rules.
pub fn foreign_key_operand_type_with_control(
    referencing: &ColumnType,
    referenced: &ColumnType,
    control: &ProductionControl<'_>,
) -> Result<Produced<ColumnType>, SQLError> {
    inline(foreign_key_operand_type_with_work(
        referencing,
        referenced,
        control,
        &mut ReferenceWork {
            control,
            work: &mut (),
        },
    ))?
    .ok_or_else(|| undefined_equality_operator(base_type(referencing), base_type(referenced)))
}

pub(crate) async fn foreign_key_operand_type_with_work<W: ColumnTypeInputWork<Error = SQLError>>(
    referencing: &ColumnType,
    referenced: &ColumnType,
    control: &ProductionControl<'_>,
    work: &mut W,
) -> Result<Option<Produced<ColumnType>>, SQLError> {
    control.check()?;
    let referencing = base_type_with_work(referencing, control, work).await?;
    let referenced = base_type_with_work(referenced, control, work).await?;
    let Some(comparison) = equality_with_work(referencing, referenced, control, work).await? else {
        return Ok(None);
    };
    let referencing_name = canonical_column_type_name_with_work(referencing, control, work).await?;
    let referenced_name = canonical_column_type_name_with_work(referenced, control, work).await?;
    let shared_operator_family = is_integral(referencing) && is_integral(referenced)
        || is_float(referencing) && is_float(referenced)
        || is_date_timestamp(referencing) && is_date_timestamp(referenced)
        || referencing.is_character_string() && referenced.is_character_string();
    if implicit_cast_with_work(&referencing_name, &referenced_name, control, work).await?
        || shared_operator_family
    {
        return Ok(Some(comparison));
    }
    ColumnType::retire_with_work(comparison, control, work).await?;
    Ok(None)
}

fn is_integral(ty: &ColumnType) -> bool {
    matches!(
        ty,
        ColumnType::SmallInteger | ColumnType::Integer | ColumnType::BigInteger
    )
}

fn is_float(ty: &ColumnType) -> bool {
    matches!(ty, ColumnType::Real | ColumnType::DoublePrecision)
}

fn is_date_timestamp(ty: &ColumnType) -> bool {
    matches!(
        ty,
        ColumnType::Date | ColumnType::Timestamp | ColumnType::TimestampTz
    )
}

fn undefined_equality_operator(left: &ColumnType, right: &ColumnType) -> SQLError {
    super::undefined_binary_operator(Some(left), "=", Some(right))
}
