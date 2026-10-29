//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Volatility decisions retain temporary type normalization and namespace parsing under their caller's allowance.

use super::{
    array_element, base_type, input_output_volatility, CastMethod, ColumnType, FunctionVolatility,
    STABLE_CAST_FUNCTIONS,
};
use crate::SQLError;
use uqa_core::memory::{Produced, ProductionControl};

pub fn cast_volatility_with_control(
    source: &ColumnType,
    target: &ColumnType,
    control: &ProductionControl<'_>,
) -> Result<FunctionVolatility, SQLError> {
    control.check()?;
    let source = base_type(source).without_type_modifiers_with_control(control)?;
    let target = base_type(target).without_type_modifiers_with_control(control)?;
    if *source == *target {
        return Ok(FunctionVolatility::Immutable);
    }
    match (&*source, &*target) {
        (ColumnType::Array(source), ColumnType::Array(target)) => {
            return cast_volatility_with_control(
                array_element(source),
                array_element(target),
                control,
            )
        }
        (ColumnType::Array(_), _) | (_, ColumnType::Array(_)) => {
            return Ok(input_output_volatility(&source, &target))
        }
        _ => {}
    }
    Ok(
        match super::super::cast_catalog_entry_with_control(&source, &target, control)?
            .map(|entry| entry.method)
        {
            Some(CastMethod::Binary) => FunctionVolatility::Immutable,
            Some(CastMethod::Function { oid, .. }) if STABLE_CAST_FUNCTIONS.contains(&oid) => {
                FunctionVolatility::Stable
            }
            Some(CastMethod::Function { .. }) => FunctionVolatility::Immutable,
            Some(CastMethod::InputOutput) | None => input_output_volatility(&source, &target),
        },
    )
}

pub fn pg_catalog_builtin_type_with_control(
    name: &str,
    control: &ProductionControl<'_>,
) -> Result<Option<Produced<ColumnType>>, SQLError> {
    control.check()?;
    let parsed = match crate::parse_regtype_name_with_control(name, control) {
        Ok(Some(parsed)) => parsed,
        Err(error) if matches!(error.sqlstate(), Some("53200" | "57014")) => return Err(error),
        Ok(None) | Err(_) => return Ok(None),
    };
    if !matches!(parsed.names.as_slice(), [schema, _] if schema == "pg_catalog") {
        return Ok(None);
    }
    match ColumnType::from_sql_name_with_control(name, control) {
        Ok(ty) => Ok(Some(ty)),
        Err(error) if matches!(error.sqlstate(), Some("53200" | "57014")) => Err(error),
        Err(_) => Ok(None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use uqa_core::{memory::MemoryBudget, CancellationToken};

    #[test]
    fn builtin_identity_retains_only_result_type_storage() {
        let budget = MemoryBudget::new(1 << 20);
        let token = CancellationToken::new();
        let control = ProductionControl::new(&budget, &token, &token);
        let mut output = None;
        let allocation = allocation_counter::measure(|| {
            output = pg_catalog_builtin_type_with_control("pg_catalog.int4[]", &control).unwrap();
        });
        let output = output.unwrap();
        assert_eq!(*output, ColumnType::Array(Box::new(ColumnType::Integer)));
        assert_eq!(
            allocation.bytes_current,
            i64::try_from(budget.used()).unwrap()
        );
        assert_eq!(output.reserved_bytes(), budget.used());
        drop(output);
        assert_eq!(budget.used(), 0);
        assert!(
            pg_catalog_builtin_type_with_control("public.int4", &control)
                .unwrap()
                .is_none()
        );
        assert_eq!(budget.used(), 0);
    }

    #[test]
    fn volatility_rejects_dynamic_type_copies_before_large_allocation() {
        let source = ColumnType::Array(Box::new(ColumnType::Enum(crate::ast::EnumTypeReference {
            schema: "public".into(),
            name: "n".repeat(1 << 20),
            oid: 16_400,
            array_oid: 16_401,
        })));
        let target = ColumnType::Array(Box::new(ColumnType::Text));
        for cancel in 0..3 {
            let budget = MemoryBudget::new(4096);
            let original = CancellationToken::new();
            let invoking = CancellationToken::new();
            let control = ProductionControl::new(&budget, &original, &invoking);
            let sibling = budget.reserve(17).unwrap();
            if cancel == 1 {
                original.cancel();
            }
            if cancel == 2 {
                invoking.cancel();
            }
            let mut error = None;
            let allocation = allocation_counter::measure(|| {
                error = Some(cast_volatility_with_control(&source, &target, &control).unwrap_err());
            });
            assert_eq!(
                error.unwrap().sqlstate(),
                Some(if cancel == 0 { "53200" } else { "57014" })
            );
            assert!(allocation.bytes_total < 1 << 15);
            assert_eq!(budget.used(), sibling.bytes());
        }
    }
}
