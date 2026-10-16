//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use std::sync::Arc;
use uqa_core::{memory::MemoryBudget, CancellationToken, DatumValue, RecordFieldType, RowValue};
use uqa_sql::{catalog::type_metadata, expr::enums::EnumTypeLabels, ColumnType};

struct Catalog;

impl SQLValueCatalog for Catalog {
    fn value_type_by_oid(&self, oid: u32) -> Result<Option<ColumnType>, SQLError> {
        Ok(type_metadata::builtin_array_element(oid)
            .map(|element| ColumnType::Array(Box::new(element.clone()))))
    }
    fn enum_type_labels(&self, _: u32) -> Result<Option<Arc<EnumTypeLabels>>, SQLError> {
        Ok(None)
    }
    fn enum_label_uncommitted(&self, _: u32) -> bool {
        false
    }
    fn enum_type_name(&self, _: u32) -> Result<Option<String>, SQLError> {
        Ok(None)
    }
    fn has_enum_types(&self) -> bool {
        false
    }
}

fn row(values: Vec<Value>, ty: &ColumnType) -> Value {
    let types = vec![
        RecordFieldType {
            collation_oid: None,
            oid: type_metadata::pg_type_oid(ty) as u32,
            type_modifier: type_metadata::pg_type_modifier(ty) as i32,
        };
        values.len()
    ];
    Value::Row(RowValue::typed(values, types).unwrap())
}

#[test]
fn record_image_keys_match_postgresql_classes_in_memory_and_spill() {
    let reference: serde_json::Value = serde_json::from_str(include_str!(
        "../../../../../uqa-sql/src/expr/binary/comparison/image/postgresql.json"
    ))
    .unwrap();
    let memory = MemoryBudget::new(1024 * 1024);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&memory, &token, &token);
    for case in reference["cases"].as_array().unwrap() {
        let name = case["type"].as_str().unwrap();
        let ty = ColumnType::from_sql_name(name).unwrap();
        let values = ["left", "right"].map(|field| {
            row(
                vec![case[field].as_str().map_or(Value::Null, |text| {
                    uqa_sql::expr::cast_value(&Value::Str(text.into()), name).unwrap()
                })],
                &ty,
            )
        });
        let keys = values.each_ref().map(|value| {
            record_image_key_with_catalog_and_control(value, Some(&Catalog), &control).unwrap()
        });
        let equal = case["order"] == 0;
        assert_eq!(*keys[0] == *keys[1], equal, "{case}");
        assert_eq!(
            memory.used(),
            keys.iter().map(|key| key.capacity()).sum::<usize>()
        );
        if equal {
            assert_eq!(
                crate::distinct::spill::stable_hash(&keys[0]),
                crate::distinct::spill::stable_hash(&keys[1]),
            );
        }
        for limit in [1, 64 * 1024] {
            let mut seen = crate::distinct::SeenKeySet::new(limit, None);
            assert!(seen.insert(keys[0].to_vec()).unwrap());
            assert_eq!(seen.insert(keys[1].to_vec()).unwrap(), !equal, "{case}");
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
fn record_image_key_framing_preserves_nulls_arity_and_physical_payloads() {
    let values = [
        vec![Value::Str("ab".into()), Value::Str("c".into())],
        vec![Value::Str("a".into()), Value::Str("bc".into())],
        vec![Value::Null],
        vec![Value::Str(String::new())],
        vec![Value::Null, Value::Null],
        vec![],
    ];
    let control = ProductionControl::uncontrolled();
    let keys = values.map(|values| {
        record_image_key_with_catalog_and_control(&row(values, &ColumnType::Text), None, &control)
            .unwrap()
    });
    for (index, key) in keys.iter().enumerate() {
        assert!(keys[..index].iter().all(|previous| **previous != **key));
    }
    let value = row(vec![Value::Str("ab".into())], &ColumnType::Text);
    let expected = record_image_key_with_catalog_and_control(&value, None, &control).unwrap();
    for bytes in [vec![7, b'a', b'b'], vec![24, 0, 0, 0, b'a', b'b']] {
        let retained = row(
            vec![Value::Datum(DatumValue::new(25, 0, bytes))],
            &ColumnType::Text,
        );
        let key = record_image_key_with_catalog_and_control(&retained, None, &control).unwrap();
        assert_eq!(*key, *expected);
    }
}

#[test]
fn record_image_key_errors_release_output_and_preserve_cancellation() {
    let value = row(vec![Value::Str("payload".repeat(8192))], &ColumnType::Text);
    for limit in [0, 16, 128, 8192] {
        let memory = MemoryBudget::new(limit);
        let token = CancellationToken::new();
        let control = ProductionControl::new(&memory, &token, &token);
        let error = record_image_key_with_catalog_and_control(&value, None, &control)
            .map(drop)
            .unwrap_err();
        assert_eq!(error.sqlstate(), Some("53200"));
        assert_eq!(memory.used(), 0);
    }
    let malformed = row(
        vec![
            Value::Str("payload".repeat(8192)),
            Value::Datum(DatumValue::new(25, 0, vec![0])),
        ],
        &ColumnType::Text,
    );
    for cancellation in [0, 1, 2] {
        let memory = MemoryBudget::new(1024 * 1024);
        let statement = CancellationToken::new();
        let session = CancellationToken::new();
        if cancellation == 1 {
            statement.cancel();
        }
        if cancellation == 2 {
            session.cancel();
        }
        let control = ProductionControl::new(&memory, &statement, &session);
        let error = record_image_key_with_catalog_and_control(&malformed, None, &control)
            .map(drop)
            .unwrap_err();
        assert_eq!(
            error.sqlstate(),
            Some(if cancellation == 0 { "XX001" } else { "57014" })
        );
        assert_eq!(memory.used(), 0);
    }
}
