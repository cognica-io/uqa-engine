//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Operator volatility borrows static declarations and retains every concrete type candidate under its production scope.

use super::{
    base_type, binary_operator_name, binary_operator_types_with_control, catalog, resolution,
    undefined_binary_operator, BinaryOp, BinaryOperatorCatalogEntry, ColumnType,
    FunctionVolatility, ProductionControl, SQLError,
};

pub fn binary_operator_volatility_with_control(
    op: BinaryOp,
    left: Option<&ColumnType>,
    right: Option<&ColumnType>,
    control: &ProductionControl<'_>,
) -> Result<FunctionVolatility, SQLError> {
    let targets = binary_operator_types_with_control(op, left, right, control)?;
    for (source, target) in [(left, &targets[0]), (right, &targets[1])] {
        if let Some(source) = source {
            if crate::type_resolution::cast_volatility_with_control(source, target, control)?
                != FunctionVolatility::Immutable
            {
                return Ok(FunctionVolatility::Stable);
            }
        }
    }
    Ok(named_binary_operator_catalog_entry_with_control(
        binary_operator_name(op),
        [&targets[0], &targets[1]],
        control,
    )?
    .volatility())
}

pub(super) fn named_binary_operator_catalog_entry_with_control(
    name: &str,
    operands: [&ColumnType; 2],
    control: &ProductionControl<'_>,
) -> Result<BinaryOperatorCatalogEntry, SQLError> {
    control.check()?;
    for polymorphic in [false, true] {
        for &(candidate, left, right, result_type, oid, function_oid) in catalog::SIGNATURES {
            control.check()?;
            if candidate != name {
                continue;
            }
            let range_match = resolution::range_operands(left, right, operands.map(Some)).is_some();
            let mut matches = range_match;
            if !matches {
                matches = true;
                for (declared, actual) in [left, right].into_iter().zip(operands) {
                    let actual = base_type(actual);
                    let declared_type = resolution::catalog_type_with_control(declared, control)?;
                    let exact = declared_type.as_deref().is_some_and(|ty| {
                        crate::catalog::type_metadata::pg_type_oid(ty)
                            == crate::catalog::type_metadata::pg_type_oid(actual)
                    });
                    let generic = polymorphic
                        && match declared {
                            "anyarray" => matches!(
                                actual,
                                ColumnType::Array(_)
                                    | ColumnType::Int2Vector
                                    | ColumnType::OidVector
                            ),
                            "anyrange" => matches!(actual, ColumnType::Range(_)),
                            "anymultirange" => matches!(actual, ColumnType::Multirange(_)),
                            "anyenum" => matches!(actual, ColumnType::Enum(_)),
                            _ => false,
                        };
                    if !exact && !generic {
                        matches = false;
                        break;
                    }
                }
            }
            if matches {
                return Ok(BinaryOperatorCatalogEntry {
                    name: candidate,
                    operand_types: [left, right],
                    result_type,
                    oid,
                    function_oid,
                });
            }
        }
    }
    Err(undefined_binary_operator(
        name,
        Some(operands[0]),
        Some(operands[1]),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use uqa_core::{memory::MemoryBudget, CancellationToken};

    #[test]
    fn controlled_operator_volatility_releases_selection_and_cast_scratch() {
        for (left, right, expected) in [
            (
                ColumnType::Integer,
                ColumnType::Integer,
                FunctionVolatility::Immutable,
            ),
            (
                ColumnType::Date,
                ColumnType::TimestampTz,
                FunctionVolatility::Stable,
            ),
            (
                ColumnType::Array(Box::new(ColumnType::Text)),
                ColumnType::Array(Box::new(ColumnType::Text)),
                FunctionVolatility::Immutable,
            ),
        ] {
            let budget = MemoryBudget::new(1 << 18);
            let token = CancellationToken::new();
            let control = ProductionControl::new(&budget, &token, &token);
            let value = binary_operator_volatility_with_control(
                BinaryOp::Less,
                Some(&left),
                Some(&right),
                &control,
            )
            .unwrap();
            assert_eq!(value, expected);
            assert_eq!(budget.used(), 0);
        }
    }
}
