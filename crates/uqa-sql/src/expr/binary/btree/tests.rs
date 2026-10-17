//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use std::cmp::Ordering;
use uqa_core::{memory::MemoryBudget, CancellationToken, DatumValue};

fn binding<'a>(ty: &'a ColumnType, name: &str) -> operator_classes::BoundBTreeOperatorClass<'a> {
    operator_classes::builtin_btree_operator_classes()
        .iter()
        .find(|class| class.name == name)
        .unwrap()
        .bind_input(ty, name, &ProductionControl::uncontrolled())
        .unwrap()
}

fn physical(hex: &str, ty: &ColumnType) -> Value {
    let mut bytes = vec![0xff];
    bytes.extend(
        (0..hex.len())
            .step_by(2)
            .map(|index| u8::from_str_radix(&hex[index..index + 2], 16).unwrap()),
    );
    Value::Datum(DatumValue::new(
        type_metadata::pg_type_oid(ty) as u32,
        1,
        bytes,
    ))
}

fn compare_inputs(
    left: BTreeInput<'_>,
    right: BTreeInput<'_>,
    control: &ProductionControl<'_>,
) -> Result<Ordering, SQLError> {
    match (left, right) {
        (BTreeInput::Value(left, ty), BTreeInput::Value(right, _)) => {
            super::super::compare_selected_values_with_catalog(
                left,
                right,
                Some(ty),
                control,
                None,
                None,
            )
        }
        (BTreeInput::Array(left), BTreeInput::Array(right)) => {
            left.cmp_by_with_control(right, control, |left, right, control| {
                super::super::compare_typed_values_with_control(left, right, control)
            })
        }
        (BTreeInput::RecordImage(left), BTreeInput::RecordImage(right)) => {
            super::super::compare_record_images_with_catalog(left, right, None, control)
        }
        _ => panic!("one selected input domain"),
    }
}

#[test]
fn selected_btree_input_observation_matches_postgresql_binary_interpretation() {
    let reference: serde_json::Value =
        serde_json::from_str(include_str!("postgresql.json")).unwrap();
    let memory = MemoryBudget::new(65536);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&memory, &token, &token);
    for case in reference["cases"].as_array().unwrap() {
        let name = case["type"].as_str().unwrap();
        let ty = ColumnType::from_sql_name(name).unwrap();
        let binding = binding(&ty, case["operator_class"].as_str().unwrap());
        let values = ["left", "right"].map(|field| {
            crate::expr::cast_value(&Value::Str(case[field].as_str().unwrap().into()), name)
                .unwrap()
        });
        let physical = ["left_datum_hex", "right_datum_hex"]
            .map(|field| physical(case[field].as_str().unwrap(), &ty));
        for left in [&values[0], &physical[0]] {
            for right in [&values[1], &physical[1]] {
                let actual = with_btree_input(binding, left, None, &control, |left| {
                    with_btree_input(binding, right, None, &control, |right| {
                        compare_inputs(left, right, &control)
                    })
                })
                .unwrap();
                assert_eq!(actual, case["order"].as_i64().unwrap().cmp(&0), "{case}");
                assert_eq!(memory.used(), 0);
            }
        }
    }
}

#[test]
fn selected_btree_input_observation_borrows_vectors_and_preserves_leases() {
    let token = CancellationToken::new();
    let empty = MemoryBudget::new(0);
    let control = ProductionControl::new(&empty, &token, &token);
    let value = crate::expr::cast_value(&Value::Str("1 20".into()), "oidvector").unwrap();
    let Value::LegacyVector(vector) = &value else {
        panic!("vector");
    };
    with_btree_input(
        binding(&ColumnType::OidVector, "array_ops"),
        &value,
        None,
        &control,
        |input| {
            let BTreeInput::Array(array) = input else {
                panic!("array input");
            };
            assert!(std::ptr::eq(array, vector.as_array()));
            Ok(())
        },
    )
    .unwrap();
    with_btree_input(
        binding(&ColumnType::Integer, "oid_ops"),
        &Value::Int(-1),
        None,
        &control,
        |input| {
            let BTreeInput::Value(value, ty) = input else {
                panic!("scalar input");
            };
            assert_eq!(value, &Value::Int(i64::from(u32::MAX)));
            assert_eq!(ty, &ColumnType::Oid);
            Ok(())
        },
    )
    .unwrap();
    assert_eq!(empty.peak(), 0);
    let text = Value::Str("a ".repeat(2048));
    let ty = ColumnType::Varchar(None);
    let selected = binding(&ty, "bpchar_ops");
    assert_eq!(
        with_btree_input(selected, &text, None, &control, |_| Ok(()))
            .unwrap_err()
            .sqlstate(),
        Some("53200")
    );
    assert_eq!(empty.used(), 0);
    let memory = MemoryBudget::new(8192);
    let control = ProductionControl::new(&memory, &token, &token);
    for fail in [false, true] {
        let result = with_btree_input(selected, &text, None, &control, |input| {
            let BTreeInput::Value(Value::FixedChar(text), _) = input else {
                panic!("character input");
            };
            assert_eq!(memory.used(), text.capacity());
            if fail {
                Err(SQLError::Internal("consumer failure".into()))
            } else {
                Ok(())
            }
        });
        assert_eq!(result.is_err(), fail);
        assert_eq!(memory.used(), 0);
    }
    for external in [false, true] {
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        if external {
            original.cancel();
        } else {
            invoking.cancel();
        }
        let control = ProductionControl::new(&memory, &original, &invoking);
        let error = with_btree_input::<()>(selected, &text, None, &control, |_| {
            panic!("cancelled input observed")
        })
        .unwrap_err();
        assert_eq!(error.sqlstate(), Some("57014"));
        assert_eq!(memory.used(), 0);
    }
    for cancel_original in [false, true] {
        for input in [&text, &Value::Null] {
            let original = CancellationToken::new();
            let invoking = CancellationToken::new();
            let control = ProductionControl::new(&memory, &original, &invoking);
            let error = with_btree_input(selected, input, None, &control, |_| {
                if cancel_original {
                    original.cancel();
                } else {
                    invoking.cancel();
                }
                Ok(())
            })
            .unwrap_err();
            assert_eq!(error.sqlstate(), Some("57014"));
            assert_eq!(memory.used(), 0);
        }
    }
}
