//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::{memory::MemoryBudget, CancellationToken, DatumValue, RecordFieldType, RowValue};
use uqa_sql::{catalog::index::operator_classes, ColumnType};

fn binding<'a>(ty: &'a ColumnType, name: &str) -> BoundBTreeOperatorClass<'a> {
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
        uqa_sql::catalog::type_metadata::pg_type_oid(ty) as u32,
        1,
        bytes,
    ))
}

#[test]
fn selected_btree_input_keys_preserve_postgresql_classes_through_spill() {
    let reference: serde_json::Value = serde_json::from_str(include_str!(
        "../../../../../uqa-sql/src/expr/binary/btree/postgresql.json"
    ))
    .unwrap();
    let memory = MemoryBudget::new(65536);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&memory, &token, &token);
    for case in reference["cases"].as_array().unwrap() {
        let name = case["type"].as_str().unwrap();
        let ty = ColumnType::from_sql_name(name).unwrap();
        let selected = binding(&ty, case["operator_class"].as_str().unwrap());
        let values = ["left", "right"].map(|field| {
            uqa_sql::expr::cast_value(&Value::Str(case[field].as_str().unwrap().into()), name)
                .unwrap()
        });
        let keys = values.each_ref().map(|value| {
            btree_input_binary_key_with_catalog_and_control(selected, value, None, &control)
                .unwrap()
        });
        let equal = case["order"] == 0;
        assert_eq!(*keys[0] == *keys[1], equal, "{case}");
        for (field, expected) in ["left_datum_hex", "right_datum_hex"].into_iter().zip(&keys) {
            let value = physical(case[field].as_str().unwrap(), &ty);
            let actual =
                btree_input_binary_key_with_catalog_and_control(selected, &value, None, &control)
                    .unwrap();
            assert_eq!(*actual, **expected, "{case}");
        }
        assert_eq!(
            memory.used(),
            keys.iter().map(|key| key.capacity()).sum::<usize>()
        );
        for limit in [1, 65536] {
            let mut seen = crate::distinct::SeenKeySet::new(limit, None);
            assert!(seen.insert(keys[0].to_vec()).unwrap());
            assert_eq!(seen.insert(keys[1].to_vec()).unwrap(), !equal);
            assert!(seen.contains(&keys[0]).unwrap());
            assert!(seen.contains(&keys[1]).unwrap());
            assert_eq!(seen.has_spilled(), limit == 1);
            assert!(seen.in_memory_bytes() <= limit);
        }
        drop(keys);
        assert_eq!(memory.used(), 0);
    }
}

#[test]
fn selected_btree_input_keys_share_array_and_record_image_owners() {
    let token = CancellationToken::new();
    let memory = MemoryBudget::new(65536);
    let control = ProductionControl::new(&memory, &token, &token);
    let value = uqa_sql::expr::cast_value(&Value::Str("1 20".into()), "oidvector").unwrap();
    let Value::LegacyVector(vector) = &value else {
        panic!("vector");
    };
    let ordinary = Value::Array(vector.as_array().clone());
    let key = btree_input_binary_key_with_catalog_and_control(
        binding(&ColumnType::OidVector, "array_ops"),
        &value,
        None,
        &control,
    )
    .unwrap();
    let expected = super::super::canonical_row_key_with_catalog_and_control(
        std::iter::once(Some(&ordinary)),
        None,
        &control,
    )
    .unwrap();
    assert_eq!(*key, *expected);
    drop((key, expected));
    assert_eq!(memory.used(), 0);
    let rows = ["1.0", "1.00"].map(|text| {
        Value::Row(
            RowValue::typed(
                vec![uqa_sql::expr::cast_value(&Value::Str(text.into()), "numeric").unwrap()],
                vec![RecordFieldType {
                    oid: 1700,
                    type_modifier: -1,
                }],
            )
            .unwrap(),
        )
    });
    let selected = binding(&ColumnType::Record, "record_image_ops");
    let keys = rows.each_ref().map(|row| {
        btree_input_binary_key_with_catalog_and_control(selected, row, None, &control).unwrap()
    });
    assert_ne!(*keys[0], *keys[1]);
    for (row, key) in rows.iter().zip(&keys) {
        let expected =
            super::super::record_image_key_with_catalog_and_control(row, None, &control).unwrap();
        assert_eq!(**key, *expected);
    }
    drop(keys);
    assert_eq!(memory.used(), 0);
    let text = Value::Str("x ".repeat(4096));
    let ty = ColumnType::Varchar(None);
    let selected = binding(&ty, "bpchar_ops");
    for limit in [0, 1, 8192] {
        let limited = MemoryBudget::new(limit);
        let control = ProductionControl::new(&limited, &token, &token);
        let error =
            btree_input_binary_key_with_catalog_and_control(selected, &text, None, &control)
                .map(drop)
                .unwrap_err();
        assert_eq!(error.sqlstate(), Some("53200"));
        assert_eq!(limited.used(), 0);
    }
    for cancel_original in [false, true] {
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        if cancel_original {
            original.cancel();
        } else {
            invoking.cancel();
        }
        let control = ProductionControl::new(&memory, &original, &invoking);
        let error =
            btree_input_binary_key_with_catalog_and_control(selected, &text, None, &control)
                .map(drop)
                .unwrap_err();
        assert_eq!(error.sqlstate(), Some("57014"));
        assert_eq!(memory.used(), 0);
    }
}
