//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::{memory::MemoryBudget, CancellationToken};

#[test]
fn enum_array_literal_admits_parsing_conversion_and_exact_retained_output() {
    let catalog = Catalog(Arc::new(EnumTypeLabels {
        type_oid: 16_400,
        labels: vec![EnumTypeLabel {
            oid: 16_402,
            key: EnumLabelKey::from_bytes(vec![64]).unwrap(),
            label: "blue".into(),
        }],
    }));
    let target = ColumnType::Array(Box::new(ColumnType::Enum(crate::ast::EnumTypeReference {
        schema: "public".into(),
        name: "label_type".into(),
        oid: 16_400,
        array_oid: 16_401,
    })));
    let input = Value::Str("[-1:0][2:3]={{blue,NULL},{blue,blue}}".into());
    let budget = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let mut output = None;
    let allocation = allocation_counter::measure(|| {
        output = Some(
            fold_unknown_literal_with_control(Some(&catalog), &input, &target, &control)
                .unwrap()
                .unwrap(),
        );
    });
    let output = output.unwrap();
    assert_eq!(
        allocation.bytes_current,
        i64::try_from(budget.used()).unwrap()
    );
    assert_eq!(output.reserved_bytes(), budget.used());
    let Value::Array(array) = &*output else {
        panic!("enum array")
    };
    assert_eq!(array.dimensions(), &[2, 2]);
    assert_eq!(array.lower_bounds(), &[-1, 2]);
    assert_eq!(array.element_type_oid(), Some(16_400));
    let Value::List(first) = &array.elements()[0] else {
        panic!("first dimension")
    };
    assert!(
        matches!(&first[0], Value::Enum(label) if label.type_oid() == 16_400 && label.label_oid() == Some(16_402))
    );
    assert_eq!(first[1], Value::Null);
    drop(output);
    assert_eq!(budget.used(), 0);
}

#[test]
fn enum_array_literal_rejects_before_large_growth_and_releases_partial_conversion() {
    let catalog = Catalog(Arc::new(EnumTypeLabels {
        type_oid: 16_400,
        labels: vec![EnumTypeLabel {
            oid: 16_402,
            key: EnumLabelKey::from_bytes(vec![64]).unwrap(),
            label: "blue".into(),
        }],
    }));
    let target = ColumnType::Array(Box::new(ColumnType::Enum(crate::ast::EnumTypeReference {
        schema: "public".into(),
        name: "label_type".into(),
        oid: 16_400,
        array_oid: 16_401,
    })));
    let input = Value::Str(format!("{{{}}}", "blue,".repeat(1 << 18)));
    for mode in 0..3 {
        let budget = MemoryBudget::new(4096);
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        let control = ProductionControl::new(&budget, &original, &invoking);
        let sibling = budget.reserve(37).unwrap();
        if mode == 1 {
            original.cancel();
        }
        if mode == 2 {
            invoking.cancel();
        }
        let mut result = None;
        let allocation = allocation_counter::measure(|| {
            result = Some(fold_unknown_literal_with_control(
                Some(&catalog),
                &input,
                &target,
                &control,
            ));
        });
        assert_eq!(
            result.unwrap().unwrap_err().sqlstate(),
            Some(if mode == 0 { "53200" } else { "57014" })
        );
        assert!(allocation.bytes_total < 32 * 1024, "{allocation:?}");
        assert_eq!(budget.used(), sibling.bytes());
        drop(sibling);
    }
    let budget = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let invalid = Value::Str("{blue,missing}".into());
    assert_eq!(
        fold_unknown_literal_with_control(Some(&catalog), &invalid, &target, &control)
            .unwrap_err()
            .sqlstate(),
        Some("22P02")
    );
    assert_eq!(budget.used(), 0);
    let recovered = fold_unknown_literal_with_control(
        Some(&catalog),
        &Value::Str("{blue}".into()),
        &target,
        &control,
    )
    .unwrap()
    .unwrap();
    drop(recovered);
    assert_eq!(budget.used(), 0);
}

struct Catalog(Arc<EnumTypeLabels>);

impl EnumLabelCatalog for Catalog {
    fn enum_type_labels(&self, oid: u32) -> Result<Option<Arc<EnumTypeLabels>>> {
        Ok((oid == self.0.type_oid).then(|| Arc::clone(&self.0)))
    }

    fn enum_label_uncommitted(&self, _: u32) -> bool {
        false
    }

    fn enum_type_name(&self, oid: u32) -> Result<Option<String>> {
        Ok((oid == self.0.type_oid).then(|| "label_type".into()))
    }

    fn has_enum_types(&self) -> bool {
        true
    }
}

#[test]
fn enum_casts_admit_selected_payloads_and_preserve_lazy_physical_inputs() {
    let labels = Arc::new(EnumTypeLabels {
        type_oid: 16_400,
        labels: vec![EnumTypeLabel {
            oid: 16_402,
            key: EnumLabelKey::from_bytes(vec![64]).unwrap(),
            label: "current label".into(),
        }],
    });
    let target = ColumnType::Enum(crate::ast::EnumTypeReference {
        schema: "public".into(),
        name: "label_type".into(),
        oid: labels.type_oid,
        array_oid: 16_401,
    });
    let native = labels.value(&labels.labels[0]);
    let catalog = Catalog(labels);
    let token = CancellationToken::new();
    for source in [
        Value::Str("current label".into()),
        native.clone(),
        Value::Datum(uqa_core::DatumValue::new(16_400, u32::MAX, vec![1])),
    ] {
        let expected = if matches!(source, Value::Str(_)) {
            &native
        } else {
            &source
        };
        let available = MemoryBudget::new(4096);
        let retained = expected.retained_payload_bytes(&available, &token).unwrap();
        for allowance in [retained - 1, retained] {
            let budget = MemoryBudget::new(allowance);
            let control = ProductionControl::new(&budget, &token, &token);
            let result =
                cast_to_enum_with_control(Some(&catalog), &source, None, &target, &control);
            if allowance < retained {
                assert_eq!(result.unwrap_err().sqlstate(), Some("53200"));
            } else {
                let value = result.unwrap().unwrap();
                assert!(value.has_same_representation(expected));
                assert_eq!(value.reserved_bytes(), retained);
                assert_eq!(budget.used(), retained);
            }
            assert_eq!(budget.used(), 0);
        }
    }
    let empty = MemoryBudget::new(0);
    let control = ProductionControl::new(&empty, &token, &token);
    let value = cast_to_enum_with_control(None, &Value::Null, None, &target, &control)
        .unwrap()
        .unwrap();
    assert_eq!(*value, Value::Null);
    assert_eq!(empty.used(), 0);
}

#[test]
fn selected_enum_production_retains_key_and_indirect_metadata() {
    let labels = EnumTypeLabels {
        type_oid: 16_400,
        labels: vec![EnumTypeLabel {
            oid: 16_402,
            key: EnumLabelKey::from_bytes(vec![64]).unwrap(),
            label: "current label".into(),
        }],
    };
    let original = CancellationToken::new();
    let invoking = CancellationToken::new();
    let retained = EnumValue::retained_header_bytes() + 1;
    for allowance in [retained - 1, retained] {
        let budget = MemoryBudget::new(allowance);
        let control = ProductionControl::new(&budget, &original, &invoking);
        let result = labels.value_with_control(&labels.labels[0], &control);
        if allowance < retained {
            assert_eq!(result.unwrap_err().sqlstate(), Some("53200"));
        } else {
            let value = result.unwrap();
            assert_eq!(value.reserved_bytes(), retained);
            assert_eq!(budget.used(), retained);
            assert!(value.has_same_representation(&labels.value(&labels.labels[0])));
        }
        assert_eq!(budget.used(), 0);
    }
    for cancel_original in [false, true] {
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        if cancel_original {
            original.cancel();
        } else {
            invoking.cancel();
        }
        let budget = MemoryBudget::new(retained);
        let control = ProductionControl::new(&budget, &original, &invoking);
        assert_eq!(
            labels
                .value_with_control(&labels.labels[0], &control)
                .unwrap_err()
                .sqlstate(),
            Some("57014")
        );
        assert_eq!(budget.used(), 0);
    }
}
