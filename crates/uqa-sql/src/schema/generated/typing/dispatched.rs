//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Generation types of structurally dispatched calls: parser-owned syntax and overload-specific built-ins.

use super::{
    column_generation_type, function_type_error, generation_type_name, type_rules, GenerationType,
};
use crate::ast::{EnumFunctionOperation, FunctionDispatch, RangeFunctionOperation, RangeSubtype};
use crate::{ColumnType, SQLError};

pub(super) fn infer_dispatched_function(
    dispatch: FunctionDispatch,
    arguments: &[GenerationType],
) -> Result<Option<GenerationType>, SQLError> {
    let first = || {
        arguments.first().cloned().ok_or_else(|| {
            SQLError::TypeMismatch(format!("{} requires an argument", dispatch.label()))
        })
    };
    Ok(Some(match dispatch {
        FunctionDispatch::NumericOperator(operator) => {
            let types = arguments
                .iter()
                .map(|ty| {
                    if type_rules::is_unknown(ty) {
                        Ok(None)
                    } else {
                        ColumnType::from_sql_name(&generation_type_name(ty)).map(Some)
                    }
                })
                .collect::<Result<Vec<_>, _>>()?;
            let selected = crate::type_resolution::numeric_operator_types(operator, &types)?;
            column_generation_type(&selected.result)
        }
        FunctionDispatch::JsonExtract { as_text, .. } => match first()? {
            input @ (GenerationType::Json | GenerationType::JsonB) => {
                if as_text {
                    GenerationType::Text
                } else {
                    input
                }
            }
            other => {
                return Err(function_type_error(
                    dispatch.label(),
                    &other,
                    "json or jsonb",
                ))
            }
        },
        // Field selection needs the composite catalog; the caller types it.
        FunctionDispatch::NamedArgument
        | FunctionDispatch::VariadicArgument
        | FunctionDispatch::FieldSelect => return Ok(None),
        FunctionDispatch::ArraySubscripts | FunctionDispatch::Subscript => match first()? {
            GenerationType::Array(element) => *element,
            GenerationType::Vector | GenerationType::Tensor => GenerationType::Real,
            GenerationType::Null | GenerationType::UnknownLiteral(_) => GenerationType::Null,
            other => {
                return Err(function_type_error(dispatch.label(), &other, "an array"));
            }
        },
        FunctionDispatch::ArraySlices
        | FunctionDispatch::Slice
        | FunctionDispatch::ArraySortJson => first()?,
        FunctionDispatch::AnyOperator
        | FunctionDispatch::AllOperator
        | FunctionDispatch::IsDistinct
        | FunctionDispatch::BetweenSymmetric => GenerationType::Boolean,
        FunctionDispatch::ToBinInt4
        | FunctionDispatch::ToBinInt8
        | FunctionDispatch::ToHexInt4
        | FunctionDispatch::ToHexInt8
        | FunctionDispatch::ToOctInt4
        | FunctionDispatch::ToOctInt8 => GenerationType::Text,
        FunctionDispatch::RandomInt4Range => GenerationType::Integer,
        FunctionDispatch::RandomInt8Range => GenerationType::BigInteger,
        FunctionDispatch::RandomNumericRange => GenerationType::Numeric,
        FunctionDispatch::Range {
            operation, subtype, ..
        } => range_function_type(operation, subtype),
        FunctionDispatch::Enum { operation, .. } => {
            enum_function_type(dispatch.label(), operation, arguments)?
        }
    }))
}

fn range_function_type(operation: RangeFunctionOperation, subtype: RangeSubtype) -> GenerationType {
    match operation {
        RangeFunctionOperation::Lower | RangeFunctionOperation::Upper => {
            column_generation_type(&subtype.scalar_type())
        }
        RangeFunctionOperation::Merge => GenerationType::Range(subtype),
        RangeFunctionOperation::Multirange => GenerationType::Multirange(subtype),
        RangeFunctionOperation::IsEmpty
        | RangeFunctionOperation::LowerInclusive
        | RangeFunctionOperation::UpperInclusive
        | RangeFunctionOperation::LowerInfinite
        | RangeFunctionOperation::UpperInfinite
        | RangeFunctionOperation::Overlap
        | RangeFunctionOperation::Contains
        | RangeFunctionOperation::ContainedBy
        | RangeFunctionOperation::Adjacent => GenerationType::Boolean,
    }
}

/// The result type of an `anyenum` support call, whose enum type is the type of its enum-typed argument.
pub(super) fn enum_function_type(
    label: &str,
    operation: EnumFunctionOperation,
    arguments: &[GenerationType],
) -> Result<GenerationType, SQLError> {
    let enum_type = || {
        arguments
            .iter()
            .find(|ty| matches!(ty, GenerationType::Enum(_)))
            .cloned()
            .ok_or_else(|| SQLError::TypeMismatch(format!("{label} requires an enum argument")))
    };
    Ok(match operation {
        EnumFunctionOperation::First
        | EnumFunctionOperation::Last
        | EnumFunctionOperation::Smaller
        | EnumFunctionOperation::Larger => enum_type()?,
        EnumFunctionOperation::Range | EnumFunctionOperation::BoundedRange => {
            GenerationType::Array(Box::new(enum_type()?))
        }
        EnumFunctionOperation::Compare | EnumFunctionOperation::Hash => GenerationType::Integer,
        EnumFunctionOperation::ExtendedHash => GenerationType::BigInteger,
        EnumFunctionOperation::Equal
        | EnumFunctionOperation::NotEqual
        | EnumFunctionOperation::Less
        | EnumFunctionOperation::Greater
        | EnumFunctionOperation::LessEqual
        | EnumFunctionOperation::GreaterEqual => GenerationType::Boolean,
    })
}
