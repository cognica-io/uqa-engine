//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::{
    catalog_role::RoleIdentity, memory::MemoryBudget, CancellationToken, RelationIdentity,
};
use uqa_sql::{
    catalog::composite_type::{StoredComposite, StoredCompositeAttribute},
    ColumnType,
};

fn definition(oid: u32, attribute: String) -> StoredComposite {
    StoredComposite {
        object_id: [7; 16],
        oid,
        relation_oid: oid + 1,
        array_oid: oid + 2,
        array_name: "_record".into(),
        identity: RelationIdentity::new("public", "record"),
        owner: RoleIdentity::BOOTSTRAP,
        attributes: vec![StoredCompositeAttribute {
            name: attribute,
            ty: ColumnType::Text,
            collation: None,
            collation_oid: Some(951),
            number: 1,
            dropped: false,
        }],
        usage_acl: None,
    }
}

#[test]
fn controlled_cache_miss_owns_only_the_selected_definition() {
    let registry = Arc::new(CompositeRegistry::from([
        ("public.small".into(), definition(80001, "selected".into())),
        (
            "public.unrelated".into(),
            definition(81001, "unrelated".repeat(128 * 1024)),
        ),
    ]));
    let cache = CompositeDescriptorCache::default();
    let budget = MemoryBudget::new(16 * 1024);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    control.check().unwrap();
    let mut selected = None;
    let allocation = allocation_counter::measure(|| {
        selected = cache
            .descriptor_with_control(&registry, 80001, &control)
            .unwrap();
    });
    assert!(allocation.bytes_total < 16 * 1024, "{allocation:?}");
    let selected = selected.unwrap();
    assert_eq!(selected.attributes[0].name, "selected");
    assert_eq!(selected.attributes[0].collation_oid, Some(951));
    assert_eq!(budget.used(), selected.reserved_bytes());
    assert!(cache.entry.lock().is_none());
    let retained = selected.clone();
    drop(selected);
    assert!(budget.used() > 0);
    drop(retained);
    assert_eq!(budget.used(), 0);
    assert_eq!(
        cache
            .descriptor_with_control(&registry, 81001, &control)
            .unwrap_err()
            .sqlstate(),
        Some("53200")
    );
    assert_eq!(budget.used(), 0);
}

#[test]
fn retained_cache_generation_is_shared_without_new_payload_or_cross_generation_reuse() {
    let registry = Arc::new(CompositeRegistry::from([(
        "public.record".into(),
        definition(80001, "original".into()),
    )]));
    let cache = CompositeDescriptorCache::default();
    let original = cache.descriptor(&registry, 80001).unwrap();
    let budget = MemoryBudget::new(0);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    control.check().unwrap();
    let allocation = allocation_counter::measure(|| {
        let selected = cache
            .descriptor_with_control(&registry, 80001, &control)
            .unwrap()
            .unwrap();
        assert!(std::ptr::eq(&raw const *selected, &raw const *original));
        assert_eq!(selected.reserved_bytes(), 0);
    });
    assert_eq!(allocation.count_total, 0);
    let changed = Arc::new(CompositeRegistry::from([(
        "public.record".into(),
        definition(80001, "changed".into()),
    )]));
    assert_eq!(
        cache
            .descriptor_with_control(&changed, 80001, &control)
            .unwrap_err()
            .sqlstate(),
        Some("53200")
    );
    assert_eq!(
        cache
            .descriptor_with_control(&registry, 80001, &control)
            .unwrap()
            .unwrap()
            .attributes[0]
            .name,
        "original"
    );
    assert_eq!(budget.used(), 0);
}
