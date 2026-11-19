//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::catalog::type_definitions::{insert_stored_entry, TypeDefinitions};
use uqa_core::memory::Budgeted;

fn key(name: &str, memory: &MemoryBudget, token: &CancellationToken) -> Budgeted<String> {
    ProductionControl::new(memory, token, token)
        .copy_text(name)
        .unwrap()
        .into_budgeted()
        .unwrap()
}
fn enum_value(memory: &MemoryBudget, token: &CancellationToken) -> Budgeted<StoredEnum> {
    StoredEnum::from_stored_json_with_control(
        &enumeration(),
        &ProductionControl::new(memory, token, token),
    )
    .unwrap()
    .into_budgeted()
    .unwrap()
}
fn shell_value(memory: &MemoryBudget, token: &CancellationToken) -> Budgeted<StoredShellType> {
    StoredShellType::from_stored_json_with_control(
        &shell(),
        &ProductionControl::new(memory, token, token),
    )
    .unwrap()
    .into_budgeted()
    .unwrap()
}

#[test]
fn stored_type_adoption_preserves_native_buffers_captured_predecessors_and_future_mutation_ownership(
) {
    let memory = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let mut registry = TypeDefinitions::with_budget(&memory);
    let value = enum_value(&memory, &token);
    let name = key("public.mood", &memory, &token);
    let name_ptr = name.as_ptr();
    let schema_ptr = value.identity.schema.as_ptr();
    let array_ptr = value.array_name.as_ptr();
    let entries_ptr = value.labels.as_ptr();
    let text_ptr = value.labels[0].label.as_ptr();
    let acl_ptr = value.usage_acl.as_ref().unwrap().as_ptr();
    let expected = serde_json::to_value(&*value).unwrap();
    insert_stored_entry(&mut registry, name, value).unwrap();
    assert_eq!(registry.iter().next().unwrap().0.as_ptr(), name_ptr);
    let first = &registry["public.mood"];
    assert_eq!(first.identity.schema.as_ptr(), schema_ptr);
    assert_eq!(first.array_name.as_ptr(), array_ptr);
    assert_eq!(first.labels.as_ptr(), entries_ptr);
    assert_eq!(first.labels[0].label.as_ptr(), text_ptr);
    assert_eq!(first.usage_acl.as_ref().unwrap().as_ptr(), acl_ptr);
    assert_eq!(serde_json::to_value(first).unwrap(), expected);
    let capture_memory = MemoryBudget::new(1 << 20);
    let captured = registry
        .capture(&ProductionControl::new(&capture_memory, &token, &token))
        .unwrap();
    let predecessor = registry.clone();
    let replacement = enum_value(&memory, &token)
        .try_map(|mut value| {
            value.oid = 24000;
            Ok::<_, SQLError>(value)
        })
        .unwrap();
    insert_stored_entry(
        &mut registry,
        key("public.mood", &memory, &token),
        replacement,
    )
    .unwrap();
    assert_eq!(predecessor["public.mood"].oid, 20000);
    assert_eq!(captured.get("public.mood").unwrap().oid, 20000);
    assert_eq!(registry["public.mood"].oid, 24000);
    assert_eq!(
        captured.get("public.mood").unwrap().labels.as_ptr(),
        entries_ptr
    );
    let mutations = MemoryBudget::new(1 << 20);
    let mut registry = registry.into_mutation_budget(&mutations);
    let occupied = memory.reserve(memory.available()).unwrap();
    token.cancel();
    registry
        .edit("public.mood", |value| {
            value.labels[0].label = "later".into();
            Ok(())
        })
        .unwrap()
        .unwrap();
    assert_eq!(registry["public.mood"].labels[0].label, "later");
    assert_eq!(
        serde_json::to_value(captured.get("public.mood").unwrap()).unwrap(),
        expected
    );
    drop((registry, predecessor, captured));
    assert_eq!(memory.used(), occupied.bytes());
    assert_eq!(capture_memory.used(), 0);
    assert_eq!(mutations.used(), 0);
    drop(occupied);
    assert_eq!(memory.used(), 0);
}

#[test]
fn stored_type_adoption_failure_preserves_native_roots_and_temporary_capture_eligibility() {
    let memory = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let mut registry = TypeDefinitions::with_budget(&memory);
    insert_stored_entry(
        &mut registry,
        key("public.pending", &memory, &token),
        shell_value(&memory, &token),
    )
    .unwrap();
    let predecessor = registry.clone();
    let retained = memory.used();
    let name = key("public.pending", &memory, &token);
    let value = shell_value(&memory, &token);
    let occupied = memory.reserve(memory.available()).unwrap();
    assert_eq!(
        insert_stored_entry(&mut registry, name, value)
            .unwrap_err()
            .sqlstate(),
        Some("53200")
    );
    assert!(registry.shares_roots(&predecessor));
    assert_eq!(memory.used(), retained + occupied.bytes());
    drop(occupied);
    let mut value = serde_json::from_str::<serde_json::Value>(&shell()).unwrap();
    value["identity"]["schema"] = "pg_temp_7".into();
    let value = StoredShellType::from_stored_json_with_control(
        &value.to_string(),
        &ProductionControl::new(&memory, &token, &token),
    )
    .unwrap()
    .into_budgeted()
    .unwrap();
    insert_stored_entry(
        &mut registry,
        key("pg_temp_7.pending", &memory, &token),
        value,
    )
    .unwrap();
    assert_eq!(registry.len(), 2);
    let captures = MemoryBudget::new(1 << 20);
    let selected = registry
        .capture(&ProductionControl::new(&captures, &token, &token))
        .unwrap();
    assert_eq!(selected.len(), 1);
    assert!(selected.get("pg_temp_7.pending").is_none());
    assert!(selected.get("public.pending").is_some());
    drop((registry, predecessor, selected));
    assert_eq!(captures.used(), 0);
    assert_eq!(memory.used(), 0);
}
