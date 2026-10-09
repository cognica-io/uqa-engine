//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::expr::enums::{EnumTypeLabel, EnumTypeLabels};
use std::sync::Arc;
use uqa_core::{memory::MemoryBudget, CancellationToken, DatumValue, EnumLabelKey};

struct Catalog<'a> {
    labels: Arc<EnumTypeLabels>,
    cancel_on_middle: Option<&'a CancellationToken>,
}

impl Catalog<'_> {
    fn new() -> Self {
        Self {
            labels: Arc::new(EnumTypeLabels {
                type_oid: 16_384,
                labels: EnumLabelKey::initial(3)
                    .unwrap()
                    .into_iter()
                    .zip([(2, "first"), (5, "middle"), (4, "last")])
                    .map(|(key, (oid, label))| EnumTypeLabel {
                        key,
                        oid,
                        label: label.into(),
                    })
                    .collect(),
            }),
            cancel_on_middle: None,
        }
    }
}

impl EnumLabelCatalog for Catalog<'_> {
    fn enum_type_labels(&self, oid: u32) -> Result<Option<Arc<EnumTypeLabels>>> {
        Ok((oid == self.labels.type_oid).then(|| Arc::clone(&self.labels)))
    }
    fn enum_label_position(&self, oid: u32) -> Result<Option<(u32, usize)>> {
        Ok(self
            .labels
            .labels
            .iter()
            .position(|label| label.oid == oid)
            .map(|position| (self.labels.type_oid, position)))
    }
    fn enum_label_uncommitted(&self, oid: u32) -> bool {
        if oid == 5 {
            if let Some(token) = self.cancel_on_middle {
                token.cancel();
            }
        }
        false
    }
    fn enum_type_name(&self, _: u32) -> Result<Option<String>> {
        Ok(Some("controlled_enum".into()))
    }
    fn has_enum_types(&self) -> bool {
        true
    }
}

fn physical(oid: u32) -> Value {
    Value::Datum(DatumValue::new(16_384, 0, oid.to_le_bytes().to_vec()))
}

#[test]
fn enum_outputs_admit_label_keys_range_buffers_and_array_metadata() {
    use EnumFunctionOperation as Op;
    let catalog = Catalog::new();
    let budget = MemoryBudget::new(4096);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    for (operation, arguments, expected) in [
        (Op::First, vec![Value::Null], vec![2]),
        (Op::Last, vec![Value::Null], vec![4]),
        (Op::Range, vec![Value::Null], vec![2, 5, 4]),
        (Op::BoundedRange, vec![physical(5), Value::Null], vec![5, 4]),
    ] {
        let result = enum_function_value_with_control(
            Some(&catalog),
            operation,
            16_384,
            &arguments,
            None,
            &control,
        )
        .unwrap();
        let values: &[Value] = match &*result {
            Value::Array(array) => {
                assert_eq!(array.element_type_oid(), Some(16_384));
                array.elements()
            }
            value => std::slice::from_ref(value),
        };
        let actual = values
            .iter()
            .map(|value| match value {
                Value::Enum(label) => label.label_oid().unwrap(),
                _ => panic!("enum output"),
            })
            .collect::<Vec<_>>();
        assert_eq!(actual, expected);
        assert!(result.reserved_bytes() > 0);
        assert_eq!(budget.used(), result.reserved_bytes());
        drop(result);
        assert_eq!(budget.used(), 0);
    }
}

#[test]
fn enum_quota_failures_release_partial_ranges_and_preserve_selected_inputs() {
    use EnumFunctionOperation as Op;
    let catalog = Catalog::new();
    let token = CancellationToken::new();
    for limit in [0, 128] {
        let budget = MemoryBudget::new(limit);
        let control = ProductionControl::new(&budget, &token, &token);
        assert_eq!(
            enum_function_value_with_control(
                Some(&catalog),
                Op::Range,
                16_384,
                &[Value::Null],
                None,
                &control
            )
            .unwrap_err()
            .sqlstate(),
            Some("53200")
        );
        assert_eq!(budget.used(), 0);
    }
    let mut bytes = vec![0; 8192];
    bytes[..4].copy_from_slice(&1_u32.to_le_bytes());
    let input = Value::Datum(DatumValue::new(16_384, 0, bytes));
    let arguments = [input.clone(), input.clone()];
    let budget = MemoryBudget::new(512);
    let control = ProductionControl::new(&budget, &token, &token);
    for operation in [Op::Smaller, Op::Larger] {
        assert_eq!(
            enum_function_value_with_control(None, operation, 16_384, &arguments, None, &control)
                .unwrap_err()
                .sqlstate(),
            Some("53200")
        );
        assert_eq!(budget.used(), 0);
        assert!(arguments
            .iter()
            .all(|value| value.has_same_representation(&input)));
    }
}

#[test]
fn enum_inline_results_need_no_output_allocation_and_strict_null_skips_bytes() {
    use EnumFunctionOperation as Op;
    let budget = MemoryBudget::new(0);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    for (operation, arguments, expected) in [
        (Op::Equal, vec![physical(1), physical(1)], Value::Bool(true)),
        (Op::Compare, vec![physical(2), physical(4)], Value::Int(-1)),
        (Op::Hash, vec![physical(1)], Value::Int(-1_905_060_026)),
        (
            Op::Smaller,
            vec![
                Value::Null,
                Value::Datum(DatumValue::new(16_384, 0, vec![1])),
            ],
            Value::Null,
        ),
    ] {
        let result =
            enum_function_value_with_control(None, operation, 16_384, &arguments, None, &control)
                .unwrap();
        assert_eq!(*result, expected);
        assert_eq!(result.reserved_bytes(), 0);
    }
    assert_eq!(budget.used(), 0);
}

#[test]
fn enum_range_cancellation_releases_the_admitted_prefix() {
    let budget = MemoryBudget::new(4096);
    for cancel_original in [true, false] {
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        let token = if cancel_original {
            &original
        } else {
            &invoking
        };
        let catalog = Catalog {
            cancel_on_middle: Some(token),
            ..Catalog::new()
        };
        let control = ProductionControl::new(&budget, &original, &invoking);
        let error = enum_function_value_with_control(
            Some(&catalog),
            EnumFunctionOperation::Range,
            16_384,
            &[Value::Null],
            None,
            &control,
        )
        .unwrap_err();
        assert_eq!(error.sqlstate(), Some("57014"));
        assert_eq!(budget.used(), 0);
    }
}
