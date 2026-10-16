//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::{memory::MemoryBudget, CancellationToken};

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
