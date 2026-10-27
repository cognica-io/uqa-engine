//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::{memory::MemoryBudget, CancellationToken};

#[test]
fn binary_fields_match_postgresql_send_functions() {
    let reference: serde_json::Value =
        serde_json::from_str(include_str!("postgresql.json")).unwrap();
    let mut failures = Vec::new();
    for case in reference["cases"].as_array().unwrap() {
        let name = case["type"].as_str().unwrap();
        let ty = ColumnType::from_sql_name(name).unwrap();
        let value = if matches!(
            ty,
            ColumnType::Regclass
                | ColumnType::Regtype
                | ColumnType::Regnamespace
                | ColumnType::Regrole
                | ColumnType::Regproc
                | ColumnType::Regprocedure
                | ColumnType::Regcollation
        ) {
            // Catalog input resolves these numeric identities before result encoding.
            Value::Int(case["input"].as_str().unwrap().parse().unwrap())
        } else {
            crate::expr::cast_value(&Value::Str(case["input"].as_str().unwrap().into()), name)
                .unwrap_or_else(|error| panic!("{case}: {error}"))
        };
        let output = format_postgres_binary_with_catalog_and_control(
            &value,
            &ty,
            None,
            &ProductionControl::uncontrolled(),
        )
        .unwrap_or_else(|error| panic!("{case}: {error}"));
        let hex: String = output.iter().map(|byte| format!("{byte:02x}")).collect();
        if hex != case["hex"].as_str().unwrap() {
            failures.push(format!("{case}: actual {hex}"));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn binary_output_admits_scratch_and_releases_partial_fields() {
    let value = Value::Array(
        uqa_core::ArrayValue::try_new(vec![Value::Str("x".repeat(2048)), Value::Null]).unwrap(),
    );
    let ty = ColumnType::Array(Box::new(ColumnType::Text));
    for limit in [0, 8, 64, 256, 1024, 8192] {
        let memory = MemoryBudget::new(limit);
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        let control = ProductionControl::new(&memory, &original, &invoking);
        let output = format_postgres_binary_with_catalog_and_control(&value, &ty, None, &control);
        if limit < 2048 {
            assert_eq!(output.unwrap_err().sqlstate(), Some("53200"));
        } else {
            let output = output.unwrap();
            assert_eq!(output.len(), 20 + 8 + 2048);
            assert_eq!(memory.used(), output.reserved_bytes());
            drop(output);
        }
        assert_eq!(memory.used(), 0);
        for token in [&original, &invoking] {
            token.cancel();
            assert_eq!(
                format_postgres_binary_with_catalog_and_control(&value, &ty, None, &control)
                    .unwrap_err()
                    .sqlstate(),
                Some("57014")
            );
            assert_eq!(memory.used(), 0);
            token.reset();
        }
    }
}

#[test]
fn deeply_nested_binary_records_use_admitted_continuations() {
    let reference: serde_json::Value =
        serde_json::from_str(include_str!("postgresql.json")).unwrap();
    for case in reference["nested_records"].as_array().unwrap() {
        let mut value = Value::Int(7);
        for level in 0..case["depth"].as_u64().unwrap() {
            value = Value::Row(
                uqa_core::RowValue::typed(
                    vec![value],
                    vec![uqa_core::RecordFieldType {
                        oid: if level == 0 { 23 } else { 2249 },
                        type_modifier: -1,
                        collation_oid: None,
                    }],
                )
                .unwrap(),
            );
        }
        let memory = MemoryBudget::new(32 * 1024 * 1024);
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        let control = ProductionControl::new(&memory, &original, &invoking);
        let encoded = format_postgres_binary_with_catalog_and_control(
            &value,
            &ColumnType::Record,
            None,
            &control,
        )
        .unwrap();
        let hex: String = encoded.iter().map(|byte| format!("{byte:02x}")).collect();
        assert_eq!(hex, case["hex"].as_str().unwrap());
        assert_eq!(memory.used(), encoded.reserved_bytes());
        drop(encoded);
        assert_eq!(memory.used(), 0);
    }
}
