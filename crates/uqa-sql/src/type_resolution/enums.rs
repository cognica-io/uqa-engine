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
use uqa_core::memory::{Produced, ProductionControl};

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

/// Admit the selected enum metadata and array wrapper before allocating their result storage.
pub(crate) fn result_type_with_control(
    operation: EnumFunctionOperation,
    reference: &EnumTypeReference,
    control: &ProductionControl<'_>,
) -> Result<Produced<ColumnType>, SQLError> {
    control.check()?;
    let ty = match operation {
        EnumFunctionOperation::First
        | EnumFunctionOperation::Last
        | EnumFunctionOperation::Smaller
        | EnumFunctionOperation::Larger
        | EnumFunctionOperation::Range
        | EnumFunctionOperation::BoundedRange => {
            let (reference, memory) = reference.clone_with_control(control)?.into_parts();
            let ty = control.finish(ColumnType::Enum(reference), memory)?;
            return if matches!(
                operation,
                EnumFunctionOperation::Range | EnumFunctionOperation::BoundedRange
            ) {
                ColumnType::array_with_control(ty, control).map_err(Into::into)
            } else {
                Ok(ty)
            };
        }
        EnumFunctionOperation::Compare | EnumFunctionOperation::Hash => ColumnType::Integer,
        EnumFunctionOperation::ExtendedHash => ColumnType::BigInteger,
        EnumFunctionOperation::Equal
        | EnumFunctionOperation::NotEqual
        | EnumFunctionOperation::Less
        | EnumFunctionOperation::Greater
        | EnumFunctionOperation::LessEqual
        | EnumFunctionOperation::GreaterEqual => ColumnType::Boolean,
    };
    Ok(control.finish(ty, control.empty_reservation())?)
}

fn polymorphic_unknown() -> SQLError {
    SQLError::Routine {
        sqlstate: "42804".into(),
        message: "could not determine polymorphic type because input has type unknown".into(),
    }
}

/// Retain only the result's concrete enum metadata; borrowed argument types keep their original owners.
pub(super) fn function_type_with_control(
    name: &str,
    binding: Option<&FunctionBinding>,
    arguments: &[Option<&ColumnType>],
    control: &ProductionControl<'_>,
) -> Result<Option<Produced<ColumnType>>, SQLError> {
    control.check()?;
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
        EnumArgumentType::Enum(reference) => {
            result_type_with_control(operation, reference, control).map(Some)
        }
        EnumArgumentType::Unknown => Err(polymorphic_unknown()),
        EnumArgumentType::Mismatch => Ok(None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use uqa_core::{memory::MemoryBudget, CancellationToken};

    fn enum_type() -> ColumnType {
        ColumnType::Enum(EnumTypeReference {
            schema: "schema".repeat(256),
            name: "label_type".repeat(256),
            oid: 16_400,
            array_oid: 16_401,
        })
    }

    #[test]
    fn enum_result_inference_owns_only_selected_result_storage() {
        let input = enum_type();
        for name in ["enum_first", "enum_range", "hashenum"] {
            let budget = MemoryBudget::new(1 << 20);
            let token = CancellationToken::new();
            let control = ProductionControl::new(&budget, &token, &token);
            let mut output = None;
            let allocated = allocation_counter::measure(|| {
                output = function_type_with_control(name, None, &[Some(&input)], &control).unwrap();
            });
            let output = output.unwrap();
            assert_eq!(
                allocated.bytes_current,
                i64::try_from(budget.used()).unwrap()
            );
            assert_eq!(output.reserved_bytes(), budget.used());
            let expected = match name {
                "enum_first" => input.clone(),
                "enum_range" => ColumnType::Array(Box::new(input.clone())),
                "hashenum" => ColumnType::Integer,
                _ => unreachable!(),
            };
            assert_eq!(*output, expected);
            if name == "hashenum" {
                assert_eq!(budget.used(), 0);
            }
            drop(output);
            assert_eq!(budget.used(), 0);
        }
    }

    #[test]
    fn enum_result_rejection_retires_partial_metadata_and_preserves_arguments() {
        let input = enum_type();
        let ColumnType::Enum(reference) = &input else {
            unreachable!()
        };
        let retained_names = reference.schema.len() + reference.name.len();
        for allowance in [
            reference.schema.len() - 1,
            retained_names - 1,
            retained_names,
        ] {
            let budget = MemoryBudget::new(allowance + 17);
            let token = CancellationToken::new();
            let control = ProductionControl::new(&budget, &token, &token);
            let sibling = budget.reserve(17).unwrap();
            let error = function_type_with_control("enum_range", None, &[Some(&input)], &control)
                .unwrap_err();
            assert_eq!(error.sqlstate(), Some("53200"));
            assert_eq!(budget.used(), sibling.bytes());
            assert_eq!(reference.name.len(), "label_type".len() * 256);
        }
        for cancel_original in [false, true] {
            let budget = MemoryBudget::new(1 << 20);
            let original = CancellationToken::new();
            let invoking = CancellationToken::new();
            if cancel_original {
                original.cancel();
            } else {
                invoking.cancel();
            }
            let control = ProductionControl::new(&budget, &original, &invoking);
            let error = function_type_with_control("enum_range", None, &[Some(&input)], &control)
                .unwrap_err();
            assert_eq!(error.sqlstate(), Some("57014"));
            assert_eq!(budget.used(), 0);
        }
    }
}
