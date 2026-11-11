//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

fn fixture() -> String {
    let key = serde_json::json!({"name":"original", "kind":"Unique", "columns":["v"]});
    serde_json::json!({
        "dropped_attributes":[{"number":3,"type_length":-1,"type_modifier":22,"dimensions":2,"by_value":false,"alignment":"i","storage":"x","collation":100,"identity":"a","is_local":false,"inheritance_count":2}],
        "columns_declared":true,
        "checks":[{"name":"check", "expr":{"Literal":"x".repeat(1 << 20)}}],
        "foreign_keys":[{"local_columns":["v"], "ref_table":"parent", "ref_columns":["v"]}],
        "key_constraints":[key.clone()],
        "persistence":"Unlogged", "on_commit":"PreserveRows",
        "hierarchy":{"parents":["parent"],"local_columns":["v"],"partition_inherited_key_constraints":[key]},
        "catalog_oids":{"relation":40000,"row_type":40004,"array_type":40005,"rule":40006},
        "row_type_array_name":"_original"
    }).to_string()
}

fn headers() -> [usize; 6] {
    [
        size_of::<Budgeted<Vec<DroppedAttribute>>>(),
        size_of::<Budgeted<Vec<TableCheck>>>(),
        size_of::<Budgeted<Vec<ForeignKey>>>(),
        size_of::<Budgeted<Vec<TableKeyConstraint>>>(),
        size_of::<Budgeted<TableHierarchy>>(),
        size_of::<Budgeted<Option<String>>>(),
    ]
}

fn snapshot(value: &NativeTableConstraints) -> TableConstraintSet {
    TableConstraintSet {
        dropped_attributes: (*value.dropped_attributes).clone(),
        columns_declared: value.columns_declared,
        checks: (*value.checks).clone(),
        foreign_keys: (*value.foreign_keys).clone(),
        key_constraints: (*value.key_constraints).clone(),
        persistence: value.persistence,
        on_commit: value.on_commit,
        hierarchy: (*value.hierarchy).clone(),
        catalog_oids: value.catalog_oids,
        row_type_array_name: (*value.row_type_array_name).clone(),
    }
}

#[test]
fn stored_table_field_publication_moves_original_payloads_into_independent_owners() {
    for text in [fixture(), "{}".into(), "[]".into()] {
        let expected: TableConstraintSet = serde_json::from_str(&text).unwrap();
        let token = CancellationToken::new();
        let probe = MemoryBudget::new(8 << 20);
        let raw =
            crate::ast::read_stored_table_constraints_json(&text, &probe, &token, &token).unwrap();
        let limit = probe.peak() + headers().iter().sum::<usize>();
        drop(raw);
        assert_eq!(probe.used(), 0);
        let memory = MemoryBudget::new(limit);
        let stored =
            StoredTableConstraints::read_stored_json(&text, &memory, &token, &token).unwrap();
        let payloads = memory.used();
        let mut native = None;
        let allocations = allocation_counter::measure(|| {
            native = Some(stored.into_native().unwrap());
        });
        assert_eq!(allocations.count_total, 6, "{allocations:?}");
        assert_eq!(memory.used(), payloads + headers().iter().sum::<usize>());
        let native = native.unwrap();
        drop(text);
        token.cancel();
        assert_eq!(
            serde_json::to_value(snapshot(&native)).unwrap(),
            serde_json::to_value(expected).unwrap()
        );
        let checks = native.checks.clone();
        let retained = checks.retained_bytes();
        drop(native);
        assert_eq!(memory.used(), retained);
        drop(checks);
        assert_eq!(memory.used(), 0);
    }
}

#[test]
fn stored_table_key_names_move_exact_allowances_without_changing_other_fields() {
    let memory = MemoryBudget::new(8 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&memory, &token, &token);
    let mut stored =
        StoredTableConstraints::read_stored_json(&fixture(), &memory, &token, &token).unwrap();
    for location in [
        StoredKeyLocation::Declared(0),
        StoredKeyLocation::Inherited(0),
    ] {
        let previous = match location {
            StoredKeyLocation::Declared(index) => stored.key_constraints[index].name.as_ref(),
            StoredKeyLocation::Inherited(index) => {
                stored.hierarchy.partition_inherited_key_constraints[index]
                    .name
                    .as_ref()
            }
        }
        .unwrap()
        .capacity();
        let before = memory.used();
        let name = control.copy_text(&"renamed".repeat(2048)).unwrap();
        let bytes = name.reserved_bytes();
        stored.replace_key_name(location, name).unwrap();
        assert_eq!(memory.used(), before + bytes - previous);
    }
    let before = memory.used();
    let previous = stored.key_constraints[0].name.as_ref().unwrap().capacity();
    stored
        .replace_key_name(
            StoredKeyLocation::Declared(0),
            control.copy_text("short").unwrap(),
        )
        .unwrap();
    assert_eq!(memory.used(), before - previous + 5);
    assert_eq!(stored.key_constraints[0].columns, ["v"]);
    assert_eq!(stored.checks[0].name.as_deref(), Some("check"));
    let native = stored.into_native().unwrap();
    assert_eq!(native.key_constraints[0].name.as_deref(), Some("short"));
    assert_eq!(
        native.hierarchy.partition_inherited_key_constraints[0]
            .name
            .as_deref(),
        Some("renamed".repeat(2048).as_str())
    );
    drop(native);
    assert_eq!(memory.used(), 0);
}

#[test]
fn stored_table_field_publication_retires_every_denied_header_and_cancellation_owner() {
    let text = fixture();
    for completed in 0..6 {
        let memory = MemoryBudget::new(8 << 20);
        let token = CancellationToken::new();
        let stored =
            StoredTableConstraints::read_stored_json(&text, &memory, &token, &token).unwrap();
        let allowed = headers()[..completed].iter().sum::<usize>();
        let sibling = memory.reserve(memory.available() - allowed).unwrap();
        assert_eq!(stored.into_native().unwrap_err().sqlstate(), Some("53200"));
        assert_eq!(memory.used(), sibling.bytes());
        drop(sibling);
        assert_eq!(memory.used(), 0);
    }
    for cancel_original in [false, true] {
        let memory = MemoryBudget::new(8 << 20);
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        let stored =
            StoredTableConstraints::read_stored_json(&text, &memory, &original, &invoking).unwrap();
        if cancel_original {
            original.cancel();
        } else {
            invoking.cancel();
        }
        assert_eq!(stored.into_native().unwrap_err().sqlstate(), Some("57014"));
        assert_eq!(memory.used(), 0);
    }
}

#[test]
fn stored_table_key_name_rejection_preserves_the_declaration_and_original_allowance() {
    let memory = MemoryBudget::new(8 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&memory, &token, &token);
    let mut stored =
        StoredTableConstraints::read_stored_json(&fixture(), &memory, &token, &token).unwrap();
    let before = memory.used();
    let other = MemoryBudget::new(1024);
    let foreign = ProductionControl::new(&other, &token, &token)
        .copy_text("foreign")
        .unwrap();
    assert!(stored
        .replace_key_name(StoredKeyLocation::Declared(0), foreign)
        .is_err());
    assert_eq!(other.used(), 0);
    assert!(stored
        .replace_key_name(
            StoredKeyLocation::Declared(9),
            control.copy_text("missing").unwrap()
        )
        .is_err());
    assert!(stored
        .replace_key_name(
            StoredKeyLocation::Declared(0),
            ProductionControl::uncontrolled()
                .copy_text("unowned")
                .unwrap()
        )
        .is_err());
    assert_eq!(stored.key_constraints[0].name.as_deref(), Some("original"));
    assert_eq!(memory.used(), before);
    let name = control.copy_text("cancelled").unwrap();
    token.cancel();
    assert_eq!(
        stored
            .replace_key_name(StoredKeyLocation::Inherited(0), name)
            .unwrap_err()
            .sqlstate(),
        Some("57014")
    );
    assert_eq!(
        stored.hierarchy.partition_inherited_key_constraints[0]
            .name
            .as_deref(),
        Some("original")
    );
    assert_eq!(memory.used(), before);
    drop(stored);
    assert_eq!(memory.used(), 0);
}
