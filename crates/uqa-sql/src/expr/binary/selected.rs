//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Selected scalar identities follow admitted values into relational consumers.

use std::cmp::Ordering;
use uqa_core::{memory::ProductionControl, Value};

use super::Result;
use crate::{
    expr::{enums::EnumComparisonState, range::RangeComparisonValue, SQLValueCatalog},
    ColumnType, SQLError,
};

/// Compare an admitted ordering key under its selected SQL type. The caller controls top-level NULL placement; this never infers a range type from a string.
pub fn compare_selected_values_with_catalog(
    left: &Value,
    right: &Value,
    ty: Option<&ColumnType>,
    control: &ProductionControl<'_>,
    catalog: Option<&dyn SQLValueCatalog>,
    state: Option<&EnumComparisonState>,
) -> Result<Ordering> {
    if let Some(order) = selected_range_order(left, right, ty, control, catalog)? {
        return Ok(order);
    }
    super::compare_typed_values_with_enum_catalog(left, right, control, catalog, state)
}

/// Preserve selected scalar types in internal equality grouping, retaining enum identity order rather than observing labels.
pub fn compare_selected_grouping_values_with_catalog(
    left: &Value,
    right: &Value,
    ty: Option<&ColumnType>,
    control: &ProductionControl<'_>,
    catalog: Option<&dyn SQLValueCatalog>,
) -> Result<Ordering> {
    if let Some(order) = selected_range_order(left, right, ty, control, catalog)? {
        return Ok(order);
    }
    super::compare_grouping_values_with_enum_catalog(left, right, control, catalog)
}

/// Compare equality keys with their selected scalar type, including SQL grouping's equal NULLs.
pub fn equal_selected_values_with_catalog(
    left: &Value,
    right: &Value,
    ty: Option<&ColumnType>,
    control: &ProductionControl<'_>,
    catalog: Option<&dyn SQLValueCatalog>,
) -> Result<bool> {
    if let Some(order) = selected_range_order(left, right, ty, control, catalog)? {
        return Ok(order.is_eq());
    }
    super::equal_typed_values_with_enum_catalog(left, right, control, catalog)
}

/// Evaluate a selected SQL equality predicate with its invoking session. Unlike grouping keys, cross-type datetime operands can require the session timezone.
pub fn equal_selected_values_with_engine(
    left: &Value,
    right: &Value,
    ty: Option<&ColumnType>,
    control: &ProductionControl<'_>,
    engine: Option<&dyn crate::expr::EngineHook>,
) -> Result<bool> {
    let catalog = engine.map(crate::expr::EngineValueCatalog);
    if let Some(order) = selected_range_order(
        left,
        right,
        ty,
        control,
        catalog
            .as_ref()
            .map(|catalog| catalog as &dyn SQLValueCatalog),
    )? {
        return Ok(order.is_eq());
    }
    Ok(super::eval_value_comparison_truth_with_engine(
        crate::ast::BinaryOp::Equal,
        left,
        right,
        control,
        engine,
        None,
    )? == Some(true))
}

pub(super) fn selected_range_order(
    left: &Value,
    right: &Value,
    ty: Option<&ColumnType>,
    control: &ProductionControl<'_>,
    catalog: Option<&dyn SQLValueCatalog>,
) -> Result<Option<Ordering>> {
    control.check()?;
    if matches!(left, Value::Null) || matches!(right, Value::Null) {
        return Ok(None);
    }
    let Some(ty) = ty else {
        return Ok(None);
    };
    let Some(left) = RangeComparisonValue::from_type_with_control(left, ty, catalog, control)?
    else {
        return Ok(None);
    };
    let right = RangeComparisonValue::from_type_with_control(right, ty, catalog, control)?
        .ok_or_else(|| {
            SQLError::Internal("selected range comparison lost its right type".into())
        })?;
    left.cmp_with_catalog_and_control(&right, catalog, control)
        .map(Some)
}

#[cfg(test)]
mod tests {
    use super::*;
    use uqa_core::{memory::MemoryBudget, CancellationToken};

    #[test]
    fn selected_range_comparison_matches_postgresql_and_releases_scratch() {
        let reference: serde_json::Value =
            serde_json::from_str(include_str!("../range/comparison/postgresql.json")).unwrap();
        let memory = MemoryBudget::new(1024 * 1024);
        let token = CancellationToken::new();
        let control = ProductionControl::new(&memory, &token, &token);
        let mut observations = 0;
        for family in reference["families"].as_array().unwrap() {
            let name = family["kind"].as_str().unwrap();
            let ty = ColumnType::from_sql_name(name).unwrap();
            let values = family["values"]
                .as_array()
                .unwrap()
                .iter()
                .map(|v| {
                    crate::expr::cast_value(&Value::Str(v.as_str().unwrap().into()), name).unwrap()
                })
                .collect::<Vec<_>>();
            for (i, left) in values.iter().enumerate() {
                for (j, right) in values.iter().enumerate() {
                    let expected = family["comparisons"][i][j].as_i64().unwrap().cmp(&0);
                    assert_eq!(
                        compare_selected_values_with_catalog(
                            left,
                            right,
                            Some(&ty),
                            &control,
                            None,
                            None
                        )
                        .unwrap(),
                        expected,
                        "{name}: {i}/{j}"
                    );
                    assert_eq!(
                        compare_selected_grouping_values_with_catalog(
                            left,
                            right,
                            Some(&ty),
                            &control,
                            None
                        )
                        .unwrap(),
                        expected
                    );
                    assert_eq!(
                        equal_selected_values_with_catalog(left, right, Some(&ty), &control, None)
                            .unwrap(),
                        expected.is_eq()
                    );
                    assert_eq!(memory.used(), 0);
                    observations += 1;
                }
            }
        }
        assert_eq!(observations, 1264);
        let left = Value::Str("[1.0,3.00)".into());
        let right = Value::Str("[1.00,3.0)".into());
        assert!(!equal_selected_values_with_catalog(
            &left,
            &right,
            Some(&ColumnType::Text),
            &control,
            None
        )
        .unwrap());
        let ty = ColumnType::from_sql_name("numrange").unwrap();
        assert!(equal_selected_values_with_catalog(
            &Value::Null,
            &Value::Null,
            Some(&ty),
            &control,
            None
        )
        .unwrap());
        assert!(!equal_selected_values_with_catalog(
            &left,
            &Value::Null,
            Some(&ty),
            &control,
            None
        )
        .unwrap());
        token.cancel();
        assert!(compare_selected_values_with_catalog(
            &left,
            &right,
            Some(&ty),
            &control,
            None,
            None
        )
        .is_err());
        assert_eq!(memory.used(), 0);
    }
}
