//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Record operators check corresponding live attribute identities only as each field is reached.

use super::ComparisonSupport;
use crate::{
    catalog::type_metadata::pg_type_oid,
    expr::{composites::CompositeTypeDescriptor, SQLValueCatalog},
    SQLError,
};
use std::sync::Arc;
use uqa_core::{memory::ProductionControl, RecordFieldType, Value};

enum Fields<'a> {
    Named(&'a uqa_core::RecordValue),
    Anonymous(&'a uqa_core::RowValue),
}

struct Record<'a> {
    fields: Fields<'a>,
    descriptor: Option<Arc<CompositeTypeDescriptor>>,
}

impl<'a> Record<'a> {
    fn new(value: &'a Value, catalog: Option<&dyn SQLValueCatalog>) -> Result<Self, SQLError> {
        let (fields, oid) = match value {
            Value::Record(record) => (Fields::Named(record), record.type_oid()),
            Value::Row(row) => (Fields::Anonymous(row), None),
            _ => unreachable!("record operator operands"),
        };
        let descriptor = oid
            .map(|oid| {
                catalog
                    .ok_or_else(|| {
                        SQLError::Internal("named record comparison has no catalog".into())
                    })?
                    .value_composite_type(oid)?
                    .ok_or_else(|| SQLError::Routine {
                        sqlstate: "42704".into(),
                        message: format!("type with OID {oid} does not exist"),
                    })
            })
            .transpose()?;
        Ok(Self { fields, descriptor })
    }

    fn value(&self, index: usize) -> Option<&Value> {
        match self.fields {
            Fields::Named(fields) => fields.get(index).map(|(_, value)| value),
            Fields::Anonymous(fields) => fields.get(index),
        }
    }

    fn field_type(&self, index: usize) -> Option<u32> {
        if let Some(descriptor) = &self.descriptor {
            return descriptor
                .attributes
                .get(index)
                .map(|attribute| pg_type_oid(&attribute.ty) as u32);
        }
        match self.fields {
            Fields::Anonymous(fields) => fields
                .field_types()?
                .get(index)
                .map(|RecordFieldType { oid, .. }| *oid),
            Fields::Named(_) => None,
        }
    }

    fn type_name(
        &self,
        index: usize,
        oid: u32,
        catalog: Option<&dyn SQLValueCatalog>,
    ) -> Result<String, SQLError> {
        if let Some(descriptor) = &self.descriptor {
            return Ok(descriptor.attributes[index].ty.display_name());
        }
        super::support::comparison_type_name(oid, catalog)
    }
}

/// A deciding earlier field suppresses later type or arity errors, while a reached field's type is checked even if its value is NULL.
pub(super) fn compare<T>(
    left: &Value,
    right: &Value,
    catalog: Option<&dyn SQLValueCatalog>,
    control: &ProductionControl<'_>,
    support: ComparisonSupport,
    equal: T,
    mut compare: impl FnMut(&Value, &Value) -> Result<Option<T>, SQLError>,
) -> Result<T, SQLError> {
    let left = Record::new(left, catalog)?;
    let right = Record::new(right, catalog)?;
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
        if let (Some(left_oid), Some(right_oid)) = (left.field_type(index), right.field_type(index))
        {
            if left_oid != right_oid {
                return Err(SQLError::Routine {
                    sqlstate: "42804".into(),
                    message: format!(
                        "cannot compare dissimilar column types {} and {} at record column {}",
                        left.type_name(index, left_oid, catalog)?,
                        right.type_name(index, right_oid, catalog)?,
                        index + 1,
                    ),
                });
            }
            if !support.available_for_oid(left_oid, catalog, control)? {
                return Err(support.error("42883", left.type_name(index, left_oid, catalog)?));
            }
        }
        if let Some(result) = compare(left_value, right_value)? {
            return Ok(result);
        }
        index += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::expr::binary::equal_typed_values_with_enum_catalog;

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
