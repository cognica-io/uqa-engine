//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use std::collections::BTreeMap;
use uqa_core::{memory::MemoryBudget, CancellationToken};

#[test]
fn schema_roots_capture_without_enumeration_or_temporary_authority_retention() {
    for count in [1, 64, 4096] {
        let mut source = SchemaRegistry::new();
        for index in 0..count {
            let name = format!("schema_{index:04}");
            source
                .insert(name.clone(), BoundSchemaSecurity::bootstrap(&name))
                .unwrap();
        }
        let original = source.clone();
        let budget = MemoryBudget::new(16 << 20);
        let token = CancellationToken::new();
        let control = ProductionControl::new(&budget, &token, &token);
        let mut captured = None;
        let allocations = allocation_counter::measure(|| {
            captured = Some(source.capture(&control).unwrap());
        });
        assert_eq!(allocations.count_total, 0);
        let required = budget.used();
        let first = captured.take().unwrap();
        let mut temporary = BoundSchemaSecurity::with_public_privileges(true);
        let entry = temporary.acl.as_ref().unwrap()[0].clone();
        temporary.acl = Some(vec![entry; 1 << 17]);
        source.insert("pg_temp_7".into(), temporary).unwrap();
        source
            .insert(
                "pg_toast_temp_7".into(),
                BoundSchemaSecurity::bootstrap("pg_toast_temp_7"),
            )
            .unwrap();
        let exact = MemoryBudget::new(required);
        let second_control = ProductionControl::new(&exact, &token, &token);
        let allocations = allocation_counter::measure(|| {
            captured = Some(source.capture(&second_control).unwrap());
        });
        assert_eq!(allocations.count_total, 0);
        assert_eq!(exact.used(), required);
        assert_eq!(captured.as_ref().unwrap().len(), count);
        source.clear();
        drop(source);
        assert!(first.iter().eq(original.iter()));
        assert!(captured.as_ref().unwrap().iter().eq(original.iter()));
        drop((first, captured));
        assert_eq!(budget.used(), 0);
        assert_eq!(exact.used(), 0);
    }
}

#[test]
fn schema_root_publication_preserves_unrelated_grants_and_matches_legacy_capture() {
    for count in [1, 32, 512] {
        let mut source = SchemaRegistry::new();
        let mut large = BoundSchemaSecurity::with_public_privileges(true);
        let entry = large.acl.as_ref().unwrap()[0].clone();
        large.acl = Some(vec![entry; 1 << 15]);
        source.insert("large_grants".into(), large).unwrap();
        for index in 0..count {
            let name = format!("schema_{index:04}");
            source
                .insert(name.clone(), BoundSchemaSecurity::bootstrap(&name))
                .unwrap();
        }
        let original = source.clone();
        let allocations = allocation_counter::measure(|| {
            source
                .insert(
                    "selected".into(),
                    BoundSchemaSecurity::with_public_privileges(true),
                )
                .unwrap();
        });
        assert!(
            allocations.bytes_total < 32768,
            "count={count}: {allocations:?}"
        );
        assert!(
            allocations.count_total < 256,
            "count={count}: {allocations:?}"
        );
        assert!(source.shares_definition("large_grants", &original));
        for name in original.keys() {
            assert!(source.shares_definition(name, &original));
        }
        source
            .insert(
                "pg_temp_8".into(),
                BoundSchemaSecurity::bootstrap("pg_temp_8"),
            )
            .unwrap();
        let legacy: BTreeMap<_, _> = source
            .iter()
            .map(|(name, value)| (name.clone(), value.clone()))
            .collect();
        let budget = MemoryBudget::new(32 << 20);
        let token = CancellationToken::new();
        let control = ProductionControl::new(&budget, &token, &token);
        let expected = crate::catalog::security::capture::schemas(&legacy, &control).unwrap();
        let captured = source.capture(&control).unwrap();
        assert!(captured
            .iter()
            .eq(expected.iter().map(|(name, value)| (name, value))));
        source
            .edit("selected", |security| {
                security.acl = Some(Vec::new());
                Ok(())
            })
            .unwrap()
            .unwrap();
        assert_ne!(source.get("selected"), captured.get("selected"));
        assert_eq!(source.get("large_grants"), captured.get("large_grants"));
        drop((source, original, legacy, expected, captured));
        assert_eq!(budget.used(), 0);
    }
}

#[test]
fn denied_schema_publication_and_capture_preserve_original_authority() {
    let native = MemoryBudget::new(32768);
    let mut source = SchemaRegistry::with_budget(&native);
    for name in ["first", "middle", "last"] {
        source
            .insert(name.into(), BoundSchemaSecurity::bootstrap(name))
            .unwrap();
    }
    let original = source.clone();
    let blocked = native.reserve(native.available()).unwrap();
    assert!(source
        .insert(
            "middle".into(),
            BoundSchemaSecurity::with_public_privileges(true)
        )
        .is_err());
    assert!(source.remove("middle").is_err());
    assert_eq!(source, original);
    drop(blocked);
    let budget = MemoryBudget::new(32768);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let first = source.capture(&control).unwrap();
    let required = budget.used();
    let insufficient = MemoryBudget::new(required - 1);
    assert!(source
        .capture(&ProductionControl::new(&insufficient, &token, &token))
        .is_err());
    assert_eq!(insufficient.used(), 0);
    let exact = MemoryBudget::new(required);
    let second = source
        .capture(&ProductionControl::new(&exact, &token, &token))
        .unwrap();
    token.cancel();
    assert!(matches!(
        source.capture(&control),
        Err(SQLError::Cancelled(_))
    ));
    drop((source, original, first));
    assert_eq!(budget.used(), 0);
    assert_eq!(exact.used(), required);
    assert!(native.used() > 0);
    drop(second);
    assert_eq!(exact.used(), 0);
    assert_eq!(native.used(), 0);
}
