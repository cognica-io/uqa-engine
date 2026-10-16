//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Record operators check corresponding live attribute identities only as each field is reached.

use super::ComparisonSupport;
use crate::{
    expr::{SQLRecordFields, SQLValueCatalog},
    SQLError,
};
use uqa_core::{memory::ProductionControl, Value};

fn type_name(
    record: &SQLRecordFields<'_>,
    index: usize,
    oid: u32,
    catalog: Option<&dyn SQLValueCatalog>,
) -> Result<String, SQLError> {
    if let Some(ty) = record.declared_field_type(index) {
        return Ok(ty.display_name());
    }
    super::support::comparison_type_name(oid, catalog)
}

/// A deciding earlier field suppresses later type or arity errors, while a reached field's type is checked even if its value is NULL.
pub(super) fn compare<T>(
    left: &Value,
    right: &Value,
    catalog: Option<&dyn SQLValueCatalog>,
    control: &ProductionControl<'_>,
    support: ComparisonSupport,
    equal: T,
    mut compare: impl FnMut(&Value, &Value, Option<u32>) -> Result<Option<T>, SQLError>,
) -> Result<T, SQLError> {
    let left = SQLRecordFields::new(left, catalog)?;
    let right = SQLRecordFields::new(right, catalog)?;
    let mut index = 0;
    loop {
        control.check()?;
        let (left_value, right_value) = match (left.value(index), right.value(index)) {
            (Some(left), Some(right)) => (left, right),
            (None, None) => return Ok(equal),
            _ => {
                return Err(SQLError::Routine {
                    sqlstate: "42804".into(),
                    message: "cannot compare record types with different numbers of columns".into(),
                })
            }
        };
        let left_oid = left.field_type_oid(index);
        let right_oid = right.field_type_oid(index);
        if let (Some(left_oid), Some(right_oid)) = (left_oid, right_oid) {
            if left_oid != right_oid {
                return Err(SQLError::Routine {
                    sqlstate: "42804".into(),
                    message: format!(
                        "cannot compare dissimilar column types {} and {} at record column {}",
                        type_name(&left, index, left_oid, catalog)?,
                        type_name(&right, index, right_oid, catalog)?,
                        index + 1,
                    ),
                });
            }
            if !support.available_for_oid(left_oid, catalog, control)? {
                return Err(support.error("42883", type_name(&left, index, left_oid, catalog)?));
            }
        }
        if let Some(result) = compare(left_value, right_value, left_oid.or(right_oid))? {
            return Ok(result);
        }
        index += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::expr::binary::equal_typed_values_with_enum_catalog;
    use uqa_core::RecordFieldType;

    fn row(values: Vec<Value>, oids: &[u32]) -> Value {
        Value::Row(
            uqa_core::RowValue::typed(
                values,
                oids.iter()
                    .map(|oid| RecordFieldType {
                        oid: *oid,
                        type_modifier: -1,
                    })
                    .collect(),
            )
            .unwrap(),
        )
    }

    #[test]
    fn reached_types_and_lengths_follow_postgresql_record_operator_order() {
        let control = ProductionControl::uncontrolled();
        let left = row(vec![Value::Int(1), Value::Null], &[23, 700]);
        let later_mismatch = row(vec![Value::Int(2), Value::Null], &[23, 701]);
        assert!(
            !equal_typed_values_with_enum_catalog(&left, &later_mismatch, &control, None).unwrap()
        );
        let reached = row(vec![Value::Int(1), Value::Null], &[23, 701]);
        let error =
            equal_typed_values_with_enum_catalog(&left, &reached, &control, None).unwrap_err();
        assert_eq!(error.sqlstate(), Some("42804"));
        assert_eq!(
            error.to_string(),
            "cannot compare dissimilar column types real and double precision at record column 2"
        );
        let shorter = row(vec![Value::Int(1)], &[23]);
        let error =
            equal_typed_values_with_enum_catalog(&left, &shorter, &control, None).unwrap_err();
        assert_eq!(
            error.to_string(),
            "cannot compare record types with different numbers of columns"
        );
        let shorter = row(vec![Value::Int(2)], &[23]);
        assert!(!equal_typed_values_with_enum_catalog(&left, &shorter, &control, None).unwrap());
    }

    #[test]
    fn record_comparison_honors_cancellation_without_copying_fields() {
        let memory = uqa_core::memory::MemoryBudget::new(0);
        let token = uqa_core::CancellationToken::new();
        let control = ProductionControl::new(&memory, &token, &token);
        let value = row(vec![Value::Null], &[23]);
        assert!(equal_typed_values_with_enum_catalog(&value, &value, &control, None).unwrap());
        assert_eq!(memory.peak(), 0);
        token.cancel();
        assert_eq!(
            equal_typed_values_with_enum_catalog(&value, &value, &control, None)
                .unwrap_err()
                .sqlstate(),
            Some("57014")
        );
        assert_eq!(memory.used(), 0);
    }

    #[test]
    fn record_field_operator_lookup_precedes_null_equality_and_ordering() {
        let control = ProductionControl::uncontrolled();
        let value = row(vec![Value::Int(1), Value::Null], &[23, 114]);
        assert!(crate::expr::value_comparison_can_fail(&value));
        let error =
            equal_typed_values_with_enum_catalog(&value, &value, &control, None).unwrap_err();
        assert_eq!(error.sqlstate(), Some("42883"));
        assert_eq!(
            error.to_string(),
            "could not identify an equality operator for type json"
        );
        let error =
            crate::expr::binary::compare_typed_values_with_control(&value, &value, &control)
                .unwrap_err();
        assert_eq!(error.sqlstate(), Some("42883"));
        assert_eq!(
            error.to_string(),
            "could not identify a comparison function for type json"
        );
        let different = row(vec![Value::Int(2), Value::Null], &[23, 114]);
        assert!(!equal_typed_values_with_enum_catalog(&value, &different, &control, None).unwrap());
        assert!(crate::expr::binary::compare_typed_values_with_control(
            &value, &different, &control
        )
        .unwrap()
        .is_lt());
        let unknown = row(vec![Value::Null], &[705]);
        let error =
            equal_typed_values_with_enum_catalog(&unknown, &unknown, &control, None).unwrap_err();
        assert_eq!(error.sqlstate(), Some("42883"));
        assert_eq!(
            error.to_string(),
            "could not identify an equality operator for type unknown"
        );
    }

    #[test]
    fn record_null_fields_preserve_distinct_equality_and_ordering_capabilities() {
        let control = ProductionControl::uncontrolled();
        // The PostgreSQL array-record oracle captures both selected operators for these types before NULL handling.
        for (oid, equality, ordering) in [
            (1790, false, false),
            (28, true, false),
            (1033, true, false),
            (194, true, true),
            (22, true, true),
            (2278, false, false),
        ] {
            let value = row(vec![Value::Null], &[oid]);
            assert!(crate::expr::value_comparison_can_fail(&value));
            let equal = equal_typed_values_with_enum_catalog(&value, &value, &control, None);
            assert_eq!(equal.is_ok(), equality, "equality for OID {oid}");
            if let Err(error) = equal {
                assert_eq!(error.sqlstate(), Some("42883"));
            }
            let order =
                crate::expr::binary::compare_typed_values_with_control(&value, &value, &control);
            assert_eq!(order.is_ok(), ordering, "ordering for OID {oid}");
            if let Err(error) = order {
                assert_eq!(error.sqlstate(), Some("42883"));
            }
        }
    }
}
