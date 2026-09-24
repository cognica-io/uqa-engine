//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Static cast compatibility that depends on declared SQL type identity.

mod catalog;

use crate::ast::ColumnType;
use crate::SQLError;
use uqa_core::memory::{Produced, ProductionControl};

use super::common::base_type;

/// A `PostgreSQL` cast's implementation, separate from its value conversion.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CastMethod {
    Binary,
    Function { oid: i64, arguments: usize },
    InputOutput,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CastCatalogEntry {
    /// `i`, `a`, and `e` identify implicit, assignment, and explicit casts.
    pub context: u8,
    pub method: CastMethod,
}

/// Return the same cast identity used by static coercion compatibility.
#[must_use]
pub fn cast_catalog_entry(source: &ColumnType, target: &ColumnType) -> Option<CastCatalogEntry> {
    cast_catalog_entry_with_control(source, target, &ProductionControl::uncontrolled())
        .expect("ordinary cast catalog lookup")
}

pub(crate) fn cast_catalog_entry_with_control(
    source: &ColumnType,
    target: &ColumnType,
    control: &ProductionControl<'_>,
) -> Result<Option<CastCatalogEntry>, SQLError> {
    let Some((context, method, oid, arguments)) = catalog::entry(
        &cast_catalog_name_with_control(source, control)?,
        &cast_catalog_name_with_control(target, control)?,
    ) else {
        return Ok(None);
    };
    let method = match method {
        b'b' => CastMethod::Binary,
        b'f' => CastMethod::Function { oid, arguments },
        b'i' => CastMethod::InputOutput,
        _ => unreachable!("invalid static cast method"),
    };
    Ok(Some(CastCatalogEntry { context, method }))
}

/// Whether an explicit SQL cast has a `PostgreSQL` coercion path, independently of its value. NULL input does not make an otherwise missing cast valid.
#[must_use]
pub fn explicit_type_compatible(source: &ColumnType, target: &ColumnType) -> bool {
    explicit_type_compatible_with_control(source, target, &ProductionControl::uncontrolled())
        .expect("ordinary explicit cast compatibility has no resource failure")
}

pub(super) fn explicit_type_compatible_with_control(
    source: &ColumnType,
    target: &ColumnType,
    control: &ProductionControl<'_>,
) -> Result<bool, SQLError> {
    control.check()?;
    let source = base_type(source).without_type_modifiers_with_control(control)?;
    let target = base_type(target).without_type_modifiers_with_control(control)?;
    if *source == *target || embedding_input_compatible(&source, &target) {
        return Ok(true);
    }
    if catalog::context(
        &cast_catalog_name_with_control(&source, control)?,
        &cast_catalog_name_with_control(&target, control)?,
    )
    .is_some()
    {
        return Ok(true);
    }
    if let ColumnType::Array(target) = &*target {
        let source = match &*source {
            ColumnType::Array(source) => Some(source.as_ref()),
            ColumnType::Int2Vector => Some(&ColumnType::SmallInteger),
            ColumnType::OidVector => Some(&ColumnType::Oid),
            _ => None,
        };
        if let Some(source) = source {
            return explicit_type_compatible_with_control(
                array_element(source),
                array_element(target),
                control,
            );
        }
    }
    Ok(is_string_io_type(&source) || is_string_io_type(&target))
}

fn cast_catalog_name_with_control(
    ty: &ColumnType,
    control: &ProductionControl<'_>,
) -> Result<Produced<String>, SQLError> {
    match ty {
        ColumnType::InternalChar => control.copy_text("char").map_err(Into::into),
        _ => super::overload_resolution::canonical_column_type_name_with_control(ty, control)
            .map_err(Into::into),
    }
}

pub(super) fn validate_explicit_cast_with_control(
    source: Option<&ColumnType>,
    target: &ColumnType,
    control: &ProductionControl<'_>,
) -> Result<(), SQLError> {
    control.check()?;
    if let Some(source) = source {
        if !explicit_type_compatible_with_control(source, target, control)? {
            return Err(undefined_cast(source, target));
        }
    }
    Ok(())
}

/// Whether ``PostgreSQL`` assignment coercion accepts the declared source and target.
/// Domains use their base type for cast selection and retain constraint checking
/// at the value conversion boundary; array coercion applies element by element.
#[must_use]
pub fn assignment_type_compatible(source: &ColumnType, target: &ColumnType) -> bool {
    let source = base_type(source).without_type_modifiers();
    let target = base_type(target).without_type_modifiers();
    if source == target
        || is_string_io_type(&target)
        || embedding_input_compatible(&source, &target)
    {
        return true;
    }
    if let (ColumnType::Array(source), ColumnType::Array(target)) = (&source, &target) {
        return assignment_type_compatible(array_element(source), array_element(target));
    }
    let source = super::canonical_column_type_name(&source);
    let target = super::canonical_column_type_name(&target);
    if super::routine_type_accepts_implicit_cast(&source, &target) {
        return true;
    }
    let numeric = |name: &str| {
        matches!(
            name,
            "int2" | "int4" | "int8" | "float4" | "float8" | "numeric"
        )
    };
    (numeric(&source) && numeric(&target))
        || matches!(
            (source.as_str(), target.as_str()),
            ("timestamp", "date" | "time")
                | ("timestamptz", "date" | "time" | "timetz" | "timestamp")
                | ("timetz" | "interval", "time")
                | ("json", "jsonb")
                | ("jsonb", "json")
                | ("bit", "varbit")
                | ("varbit", "bit")
                | (
                    "oid" | "regclass" | "regnamespace" | "regproc" | "regrole" | "regtype",
                    "int4" | "int8"
                )
        )
}

fn array_element(mut ty: &ColumnType) -> &ColumnType {
    while let ColumnType::Array(element) = ty {
        ty = element;
    }
    ty
}

fn embedding_input_compatible(source: &ColumnType, target: &ColumnType) -> bool {
    let element = match (source, target) {
        (ColumnType::Array(element), ColumnType::Vector(_)) => element.as_ref(),
        (ColumnType::Array(rows), ColumnType::Tensor(_)) => match rows.as_ref() {
            ColumnType::Array(element) => element.as_ref(),
            _ => return false,
        },
        _ => return false,
    };
    matches!(
        base_type(element),
        ColumnType::SmallInteger
            | ColumnType::Integer
            | ColumnType::BigInteger
            | ColumnType::Real
            | ColumnType::DoublePrecision
            | ColumnType::Numeric { .. }
    )
}

fn is_string_io_type(ty: &ColumnType) -> bool {
    matches!(
        ty,
        ColumnType::Text
            | ColumnType::Name
            | ColumnType::Varchar(_)
            | ColumnType::Bpchar
            | ColumnType::Character(_)
    )
}

fn undefined_cast(source: &ColumnType, target: &ColumnType) -> SQLError {
    SQLError::Routine {
        sqlstate: "42846".into(),
        message: format!(
            "cannot cast type {} to {}",
            source.sql_name(),
            target.sql_name()
        ),
    }
}

#[cfg(test)]
mod production_tests;
