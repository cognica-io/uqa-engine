//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn stored_key_restoration_publishes_original_local_and_inherited_definitions() {
    let (mut schema, mut expected, ordinary_names) = fixture();
    expected.hierarchy.partition_inherited_key_constraints = expected.key_constraints.clone();
    schema.constraints_json = encode(&expected).unwrap();
    let expected = ordinary_names.decode(&schema).unwrap();
    let owner = MemoryBudget::new(1 << 20);
    let names = names(&row(), &owner);
    let memory = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let stored = names
        .decode_stored_with_control(&schema, &memory, &token, &token)
        .unwrap();
    assert_eq!(
        serde_json::to_value(&*stored).unwrap(),
        serde_json::to_value(&expected).unwrap()
    );
    let native = stored.into_native().unwrap();
    drop((names, schema));
    token.cancel();
    assert_eq!(owner.used(), 0);
    assert_eq!(
        serde_json::to_value(&*native.key_constraints).unwrap(),
        serde_json::to_value(&expected.key_constraints).unwrap()
    );
    assert_eq!(
        serde_json::to_value(&*native.hierarchy).unwrap(),
        serde_json::to_value(&expected.hierarchy).unwrap()
    );
    assert!(memory.used() > 0);
    drop(native);
    assert_eq!(memory.used(), 0);

    for text in ["", "{}", "[]"] {
        let (mut schema, _, _) = fixture();
        schema.constraints_json = text.into();
        let names = KeyConstraintNames { names: None };
        let token = CancellationToken::new();
        let stored = names
            .decode_stored_with_control(&schema, &memory, &token, &token)
            .unwrap();
        assert_eq!(
            serde_json::to_value(&*stored).unwrap(),
            serde_json::to_value(names.decode(&schema).unwrap()).unwrap()
        );
        drop(stored.into_native().unwrap());
        assert_eq!(memory.used(), 0);
    }
}

#[test]
fn stored_key_restoration_retires_partial_names_and_preserves_owner_diagnostics() {
    let owner = MemoryBudget::new(1 << 20);
    let names = names(&row(), &owner);
    let retained = owner.used();
    let memory = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    for defect in [
        "relation",
        "incarnation",
        "missing",
        "identity",
        "redundant",
    ] {
        let (mut schema, mut constraints, ordinary_names) = fixture();
        constraints.hierarchy.partition_inherited_key_constraints =
            constraints.key_constraints.clone();
        schema.constraints_json = encode(&constraints).unwrap();
        let mut stored: TableConstraintSet =
            serde_json::from_str(&schema.constraints_json).unwrap();
        let inherited = &mut stored.hierarchy.partition_inherited_key_constraints[0];
        match defect {
            "relation" => schema.relation.name = "other".into(),
            "incarnation" => schema.object_id = [9; 16],
            "missing" => inherited.catalog_identity.as_mut().unwrap().object_id = [9; 16],
            "identity" => inherited.catalog_identity = None,
            "redundant" => inherited.name = Some("redundant".into()),
            _ => unreachable!(),
        }
        schema.constraints_json = serde_json::to_string(&stored).unwrap();
        let expected = ordinary_names.decode(&schema).unwrap_err().to_string();
        let error = names
            .decode_stored_with_control(&schema, &memory, &token, &token)
            .unwrap_err();
        assert_eq!(error.to_string(), expected, "{defect}");
        assert_eq!(memory.used(), 0, "{defect}");
        assert_eq!(owner.used(), retained, "{defect}");
    }
    drop(names);
    assert_eq!(owner.used(), 0);
}

#[test]
fn stored_key_restoration_admits_exact_payloads_and_checks_both_cancellation_owners() {
    let (schema, _, _) = fixture();
    let owner = MemoryBudget::new(4 << 20);
    let names = names(&row(), &owner);
    let memory = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let mut restored = None;
    let allocated = allocation_counter::measure(|| {
        restored = Some(
            names
                .decode_stored_with_control(&schema, &memory, &token, &token)
                .unwrap(),
        );
    });
    assert_eq!(
        usize::try_from(allocated.bytes_current).unwrap(),
        memory.used()
    );
    assert!(usize::try_from(allocated.bytes_max).unwrap() <= memory.peak());
    let peak = memory.peak();
    drop(restored);
    assert_eq!(memory.used(), 0);
    for limit in (0..peak).step_by(37).chain([peak - 1]) {
        let limited = MemoryBudget::new(limit);
        let error = names
            .decode_stored_with_control(&schema, &limited, &token, &token)
            .unwrap_err();
        assert_eq!(
            crate::storage_errors::storage_error("restore table fields", &error).sqlstate(),
            Some("53200"),
            "{limit}/{peak}: {error}"
        );
        assert_eq!(limited.used(), 0, "{limit}/{peak}");
    }
    for cancel_original in [false, true] {
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        if cancel_original {
            original.cancel();
        } else {
            invoking.cancel();
        }
        assert!(matches!(
            names.decode_stored_with_control(&schema, &memory, &original, &invoking),
            Err(StorageBackendError::Cancelled(_))
        ));
        assert_eq!(memory.used(), 0);
    }
    drop(names);
    assert_eq!(owner.used(), 0);
    let mut large = row();
    large.relation.name = "x".repeat(1 << 20);
    let names = super::names(&large, &owner);
    let memory = MemoryBudget::new(16384);
    let allocation = allocation_counter::measure(|| {
        assert!(matches!(
            names.decode_stored_with_control(&schema, &memory, &token, &token),
            Err(StorageBackendError::Memory(_))
        ));
    });
    assert!(allocation.bytes_total < 32768, "{allocation:?}");
    assert_eq!(memory.used(), 0);
    drop(names);
    assert_eq!(owner.used(), 0);
}
