//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Overload identity normalization streams through ordinary or admitted text owners.

use crate::ast::ColumnType;
use uqa_core::{
    memory::{Produced, ProductionControl},
    ValueRetentionError,
};

/// Canonical type spelling used by routine identity and overload resolution.
#[must_use]
pub fn canonical_routine_type_name(type_name: &str) -> String {
    canonical_routine_type_name_with_control(type_name, &ProductionControl::uncontrolled())
        .expect("ordinary routine type normalization cannot be cancelled or limited")
        .into_uncontrolled()
        .expect("ordinary routine type normalization has no reservation")
}

mod input;
pub use input::RoutineSignatureInputError;

pub fn canonical_routine_type_name_with_control(
    type_name: &str,
    control: &ProductionControl<'_>,
) -> Result<Produced<String>, ValueRetentionError> {
    crate::catalog::work::inline(input::normalize(type_name, control, &mut ()))
}

/// Normalize a retained signature through its original input work channel. The native output owns the supplied production allowance independently of that channel.
pub async fn canonical_routine_type_name_from_input(
    type_name: &str,
    control: &ProductionControl<'_>,
    input: &mut uqa_core::image_io::AsyncImageInput<'_>,
) -> Result<Produced<String>, RoutineSignatureInputError> {
    input::normalize(type_name, control, input).await
}

fn array_element_spelling<'a>(
    name: &'a str,
    control: &ProductionControl<'_>,
) -> Result<Option<&'a str>, ValueRetentionError> {
    crate::catalog::work::inline(input::array_element_spelling(name, control, &mut ()))
}

#[must_use]
pub fn canonical_column_type_name(ty: &ColumnType) -> String {
    canonical_column_type_name_with_control(ty, &ProductionControl::uncontrolled())
        .expect("ordinary column type normalization cannot be cancelled or limited")
        .into_uncontrolled()
        .expect("ordinary column type normalization has no reservation")
}

/// Fixed overload spelling of a builtin scalar type, identical to normalizing its SQL name. Type modifiers never take part in overload identity.
pub(in crate::type_resolution) const fn builtin_canonical_type_name(
    ty: &ColumnType,
) -> Option<&'static str> {
    Some(match ty {
        ColumnType::SmallInteger => "int2",
        ColumnType::Integer => "int4",
        ColumnType::BigInteger => "int8",
        ColumnType::Real => "float4",
        ColumnType::DoublePrecision => "float8",
        ColumnType::Numeric { .. } => "numeric",
        ColumnType::Boolean => "bool",
        ColumnType::Text => "text",
        ColumnType::Name => "name",
        ColumnType::Varchar(_) => "varchar",
        ColumnType::Bpchar | ColumnType::Character(_) => "bpchar",
        ColumnType::Date => "date",
        ColumnType::Time | ColumnType::TimePrecision(_) => "time",
        ColumnType::TimeTz | ColumnType::TimeTzPrecision(_) => "timetz",
        ColumnType::Timestamp | ColumnType::TimestampPrecision(_) => "timestamp",
        ColumnType::TimestampTz | ColumnType::TimestampTzPrecision(_) => "timestamptz",
        ColumnType::Interval => "interval",
        ColumnType::Bytea => "bytea",
        ColumnType::Uuid => "uuid",
        ColumnType::Json => "json",
        ColumnType::Xml => "xml",
        ColumnType::JsonB => "jsonb",
        ColumnType::Oid => "oid",
        ColumnType::Bit(_) => "bit",
        ColumnType::Int2Vector => "int2vector",
        ColumnType::OidVector => "oidvector",
        ColumnType::Record => "record",
        ColumnType::Void => "void",
        _ => return None,
    })
}

pub(in crate::type_resolution) fn canonical_column_type_name_with_control(
    ty: &ColumnType,
    control: &ProductionControl<'_>,
) -> Result<Produced<String>, ValueRetentionError> {
    if let Some(name) = builtin_canonical_type_name(ty) {
        return control.copy_text(name);
    }
    // User-defined type spellings are neither unique across schemas nor stable across renames, so overload matching identifies them by OID.
    if let Some(identity) = ty.user_type_identity_with_control(control)? {
        if let Some(element) = array_element_spelling(&identity, control)? {
            return control.format(format_args!("{element}[]"));
        }
        return Ok(identity);
    }
    let name = ty.sql_name_with_control(control)?;
    canonical_routine_type_name_with_control(&name, control)
}

/// Parse a canonical enum identity produced by overload matching: the enum type OID and the number of array dimensions around it.
#[must_use]
pub fn parse_enum_type_identity(name: &str) -> Option<(u32, usize)> {
    crate::ast::UserTypeIdentity::parse(name)
        .filter(|identity| identity.kind == crate::ast::UserTypeKind::Enum)
        .map(|identity| (identity.oid, identity.dimensions))
}

#[cfg(test)]
mod tests;
