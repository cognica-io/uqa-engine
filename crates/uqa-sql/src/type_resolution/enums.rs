//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `anyenum` support-function typing: every argument declared `anyenum` must share one enum type, and `unknown` arguments take it.

use super::common::base_type;
use crate::ast::{
    ColumnType, EnumFunctionOperation, EnumTypeReference, FunctionBinding, FunctionDispatch,
};
use crate::SQLError;

/// The enum type selected by the `anyenum` arguments of one call.
pub(crate) enum EnumArgumentType<'a> {
    /// Every `anyenum` argument is `unknown`.
    Unknown,
    /// Every known `anyenum` argument has this enum type.
    Enum(&'a EnumTypeReference),
    /// A known argument is not an enum, the enum arguments disagree, or a non-enum argument has no implicit cast to its declared type.
    Mismatch,
}

/// Apply `PostgreSQL`'s polymorphic consistency rule to the `anyenum` arguments. `check_generic_type_consistency` does not flatten a domain for `anyenum`, so a domain over an enum is not an enum argument.
pub(crate) fn enum_argument_type<'a>(
    operation: EnumFunctionOperation,
    argument_types: &[Option<&'a ColumnType>],
) -> EnumArgumentType<'a> {
    let count = operation.enum_argument_count();
    let mut selected: Option<&EnumTypeReference> = None;
    for ty in argument_types.iter().take(count).flatten() {
        let ColumnType::Enum(reference) = ty else {
            return EnumArgumentType::Mismatch;
        };
        match selected {
            Some(selected) if selected.oid != reference.oid => return EnumArgumentType::Mismatch,
            _ => selected = Some(reference),
        }
    }
    if operation == EnumFunctionOperation::ExtendedHash {
        let seed_accepted = argument_types.get(1).is_some_and(|seed| {
            seed.is_none_or(|seed| {
                matches!(
                    base_type(seed),
                    ColumnType::SmallInteger | ColumnType::Integer | ColumnType::BigInteger
                )
            })
        });
        if !seed_accepted {
            return EnumArgumentType::Mismatch;
        }
    }
    selected.map_or(EnumArgumentType::Unknown, EnumArgumentType::Enum)
}

/// The result type of an enum support function for its concrete enum type.
#[must_use]
pub(crate) fn result_type(
    operation: EnumFunctionOperation,
    reference: &EnumTypeReference,
) -> ColumnType {
    match operation {
        EnumFunctionOperation::First
        | EnumFunctionOperation::Last
        | EnumFunctionOperation::Smaller
        | EnumFunctionOperation::Larger => ColumnType::Enum(reference.clone()),
        EnumFunctionOperation::Range | EnumFunctionOperation::BoundedRange => {
            ColumnType::Array(Box::new(ColumnType::Enum(reference.clone())))
        }
        EnumFunctionOperation::Compare | EnumFunctionOperation::Hash => ColumnType::Integer,
        EnumFunctionOperation::ExtendedHash => ColumnType::BigInteger,
        EnumFunctionOperation::Equal
        | EnumFunctionOperation::NotEqual
        | EnumFunctionOperation::Less
        | EnumFunctionOperation::Greater
        | EnumFunctionOperation::LessEqual
        | EnumFunctionOperation::GreaterEqual => ColumnType::Boolean,
    }
}

fn polymorphic_unknown() -> SQLError {
    SQLError::Routine {
        sqlstate: "42804".into(),
        message: "could not determine polymorphic type because input has type unknown".into(),
    }
}

/// Type a bound or unbound enum support-function call. `None` leaves an unmatched call to ordinary function resolution, which reports `42883`.
pub(super) fn function_type(
    name: &str,
    binding: Option<&FunctionBinding>,
    arguments: &[Option<&ColumnType>],
) -> Result<Option<ColumnType>, SQLError> {
    let operation = match binding {
        Some(FunctionBinding {
            dispatch: Some(FunctionDispatch::Enum { operation, .. }),
            ..
        }) => *operation,
        Some(binding) if !binding.builtin || binding.dispatch.is_some() => return Ok(None),
        _ => match EnumFunctionOperation::from_call(name, arguments.len()) {
            Some(operation) => operation,
            None => return Ok(None),
        },
    };
    match enum_argument_type(operation, arguments) {
        EnumArgumentType::Enum(reference) => Ok(Some(result_type(operation, reference))),
        EnumArgumentType::Unknown => Err(polymorphic_unknown()),
        EnumArgumentType::Mismatch => Ok(None),
    }
}
