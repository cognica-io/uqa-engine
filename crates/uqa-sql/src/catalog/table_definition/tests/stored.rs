//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::catalog::image;

fn columns() -> Vec<ColumnDef> {
    let mut value = ColumnDef::nullable("payload", crate::ColumnType::Text);
    value.default = Some(Expr::Literal(Value::Str("x".repeat(1 << 20))));
    value.missing_value = Some(Value::Str("y".repeat(1 << 20)));
    vec![value]
}

fn encoded(value: &[ColumnDef]) -> Vec<u8> {
    let memory = MemoryBudget::new(8 << 20);
    let token = CancellationToken::new();
    let mut reader = image::columns_image_reader(value, &memory, &token).unwrap();
    let mut bytes = Vec::new();
    let mut buffer = [0; 127];
    loop {
        let count = reader.read(&mut buffer).unwrap();
        if count == 0 {
            break;
        }
        bytes.extend_from_slice(&buffer[..count]);
    }
    bytes
}

#[test]
fn stored_column_generation_transfers_native_payloads_and_captures_after_source_retirement() {
    let values = columns();
    let expected = encoded(&values);
    let text = serde_json::to_string(&values).unwrap();
    let token = CancellationToken::new();
    let probe = MemoryBudget::new(8 << 20);
    let decoded = crate::ast::read_stored_columns_json(&text, &probe, &token, &token).unwrap();
    let limit = probe.peak() + size_of::<Budgeted<Vec<ColumnDef>>>();
    drop((decoded, values));
    assert_eq!(probe.used(), 0);
    let memory = MemoryBudget::new(limit);
    let original = CancellationToken::new();
    let invoking = CancellationToken::new();
    let definition =
        TableDefinition::read_stored_json(&text, &memory, &original, &invoking).unwrap();
    let retained = definition.retained_bytes();
    assert_eq!(memory.used(), retained);
    assert!(memory.peak() <= limit);
    drop(text);
    original.cancel();
    invoking.cancel();
    assert_eq!(encoded(&definition), expected);

    let independent = MemoryBudget::new(retained);
    let control = ProductionControl::new(&independent, &token, &token);
    let mut captured = None;
    let allocations = allocation_counter::measure(|| {
        captured = Some(definition.capture_with_control(&control).unwrap());
    });
    assert_eq!(allocations.count_total, 0);
    let captured = captured.unwrap();
    assert!(definition.shares_definition(&captured));
    assert_eq!(independent.used(), retained);
    let shared = definition.shared();
    drop((definition, captured));
    assert_eq!(independent.used(), 0);
    assert_eq!(memory.used(), retained);
    assert_eq!(encoded(&shared), expected);
    drop(shared);
    assert_eq!(memory.used(), 0);
}

#[test]
fn stored_column_generation_retires_both_cancellation_paths_and_denied_shared_headers() {
    let text = serde_json::to_string(&columns()).unwrap();
    for cancel_original in [false, true] {
        let memory = MemoryBudget::new(8 << 20);
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        let selected = if cancel_original {
            &original
        } else {
            &invoking
        };
        let result = crate::ast::stored_input::test_control::cancel_after_allocation(
            &memory,
            selected,
            65536,
            || TableDefinition::read_stored_json(&text, &memory, &original, &invoking),
        );
        assert_eq!(result.unwrap_err().sqlstate(), Some("57014"));
        assert_eq!(memory.used(), 0);
        assert_eq!(
            TableDefinition::read_stored_json("[]", &memory, &original, &invoking)
                .unwrap_err()
                .sqlstate(),
            Some("57014")
        );
    }
    let token = CancellationToken::new();
    let memory = MemoryBudget::new(size_of::<Budgeted<Vec<ColumnDef>>>() - 1);
    assert_eq!(
        TableDefinition::read_stored_json("[]", &memory, &token, &token)
            .unwrap_err()
            .sqlstate(),
        Some("53200")
    );
    assert_eq!(memory.peak(), 0);
    assert_eq!(memory.used(), 0);
}

#[test]
fn stored_column_generation_publication_detaches_without_changing_retained_snapshots() {
    let memory = MemoryBudget::new(8 << 20);
    let token = CancellationToken::new();
    let text = serde_json::to_string(&columns()).unwrap();
    let mut definition = TableDefinition::read_stored_json(&text, &memory, &token, &token).unwrap();
    let original = definition.clone();
    let expected = encoded(&original);
    let retained = memory.used();
    drop(text);
    token.cancel();
    TableDefinition::write(&mut definition)
        .edit(|columns| {
            columns[0].name = "changed".into();
            Ok(())
        })
        .unwrap();
    assert_eq!(definition[0].name, "changed");
    assert_eq!(original[0].name, "payload");
    assert!(!definition.shares_definition(&original));
    assert_eq!(encoded(&original), expected);
    assert_eq!(memory.used(), retained);
    drop(original);
    assert_eq!(memory.used(), 0);
    assert_eq!(definition[0].name, "changed");
}
