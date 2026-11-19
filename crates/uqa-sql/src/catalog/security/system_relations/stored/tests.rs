//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::{
    catalog_role::{BoundAclEntry, RoleIdentity},
    image_io::{ImageDecodeWindow, ImageDecoder},
    memory::MemoryBudget,
    CancellationToken,
};

const PROTOCOL: ScalarProtocol = ScalarProtocol {
    number: "test number representation",
    raw: "test raw representation",
};
fn fixture() -> String {
    serde_json::to_string(&StoredSystemAcl {
        system_acl_format: 1,
        entry: SystemAcl {
            revision: [7; 16],
            acl: vec![BoundAclEntry {
                role: None,
                grantor: RoleIdentity::BOOTSTRAP,
                privileges: TablePrivileges {
                    select: true,
                    ..TablePrivileges::default()
                },
                grant_options: TablePrivileges::default(),
            }],
        },
    })
    .unwrap()
}
fn equivalent(text: &str) {
    let memory = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let value: serde_json::Value = serde_json::from_str(text).unwrap();
    let expected = serde_json::from_value::<StoredSystemAcl<SystemAcl>>(value);
    let actual = StoredSystemAcl::from_stored_value_json(
        text,
        PROTOCOL,
        &ProductionControl::new(&memory, &token, &token),
    );
    assert_eq!(actual.is_ok(), expected.is_ok(), "{text}");
    if let (Ok(actual), Ok(expected)) = (actual, expected) {
        assert_eq!(actual.system_acl_format, expected.system_acl_format);
        assert_eq!(actual.entry, expected.entry);
    }
    assert_eq!(memory.used(), 0);
}

#[test]
fn stored_system_authority_preserves_projected_defaults_duplicates_and_positional_shapes() {
    let text = fixture();
    equivalent(&text);
    let original: serde_json::Value = serde_json::from_str(&text).unwrap();
    for pointer in [
        "",
        "/entry",
        "/entry/acl/0",
        "/entry/acl/0/grantor",
        "/entry/acl/0/privileges",
    ] {
        let keys: Vec<_> = original
            .pointer(pointer)
            .unwrap()
            .as_object()
            .unwrap()
            .keys()
            .cloned()
            .collect();
        for key in keys {
            let mut changed = original.clone();
            changed
                .pointer_mut(pointer)
                .unwrap()
                .as_object_mut()
                .unwrap()
                .remove(&key);
            equivalent(&changed.to_string());
        }
        let mut changed = original.clone();
        changed.pointer_mut(pointer).unwrap()["ignored"] = serde_json::json!([1, "값", null]);
        equivalent(&changed.to_string());
    }
    for privileges in [
        serde_json::json!([]),
        serde_json::json!([true]),
        serde_json::json!([true, false, false, false, false, false, false, true]),
        serde_json::json!([false, false, false, false, false, false, false, false, false]),
    ] {
        let mut changed = original.clone();
        changed["entry"]["acl"][0]["privileges"] = privileges;
        equivalent(&changed.to_string());
    }
    for version in ["-0", "1.0", "1e0", "4294967295", "4294967296", "null"] {
        equivalent(&text.replacen(
            "\"system_acl_format\":1",
            &format!("\"system_acl_format\":{version}"),
            1,
        ));
    }
    equivalent(&text.replacen("\"revision\":", "\"revision\":null,\"revision\":", 1));
    equivalent(&text.replacen("\"select\":true", "\"select\":null,\"select\":true", 1));
    let mut changed = original.clone();
    changed["entry"] = serde_json::json!([original["entry"]["revision"], original["entry"]["acl"]]);
    equivalent(&changed.to_string());
    changed["entry"].as_array_mut().unwrap().pop();
    equivalent(&changed.to_string());
    equivalent(&serde_json::json!([1, original["entry"]]).to_string());
}

#[test]
fn stored_system_authority_adopts_last_native_buffers_and_independent_mutation_ownership() {
    let memory = MemoryBudget::new(1 << 20);
    let mutations = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&memory, &token, &token);
    let stored = StoredSystemAcl::from_stored_value_json(&fixture(), PROTOCOL, &control)
        .unwrap()
        .into_budgeted()
        .unwrap();
    let pointer = stored.entry.acl.as_ptr() as usize;
    let (stored, lease) = stored.into_parts();
    let relation = SystemRelation::Projected(crate::catalog::VirtualRelation::PgShadow);
    let mut group = SystemRelationAssembly::new(relation);
    group
        .insert("usename", Budgeted::new(stored.entry, lease), &control)
        .unwrap();
    let mut input = ImageDecoder::<_, SQLError>::new(&memory, &token, |mut reader| async move {
        let mut work = reader.input();
        let control = ProductionControl::new(work.memory, work.cancellation, work.cancellation);
        let value = group.finish(&control, &mut work).await?;
        let mut registry = SystemRelationSecurities::with_budget(control.budget().unwrap());
        insert_stored_entry(&mut registry, relation, value, &control)?;
        Ok(registry)
    })
    .unwrap();
    let mut registry = loop {
        if let Some(value) = input
            .advance(
                &mut std::io::empty(),
                ImageDecodeWindow { bytes: 1, steps: 1 },
            )
            .unwrap()
        {
            break value;
        }
    }
    .into_mutation_budget(&mutations);
    let key = RelationIdentity::new(relation.namespace(), relation.name());
    assert_eq!(
        registry[&key].columns["usename"].acl.as_ptr() as usize,
        pointer
    );
    let original = registry.clone();
    let occupied = memory.reserve(memory.available()).unwrap();
    token.cancel();
    registry
        .insert(key.clone(), SystemRelationSecurity::default())
        .unwrap();
    assert!(!original[&key].columns["usename"].acl.is_empty());
    drop((registry, original, input));
    assert_eq!(memory.used(), occupied.bytes());
    drop(occupied);
    assert_eq!(memory.used(), 0);
    assert_eq!(mutations.used(), 0);
    for limit in [0, 1, 64] {
        let memory = MemoryBudget::new(limit);
        let token = CancellationToken::new();
        assert!(StoredSystemAcl::from_stored_value_json(
            &fixture(),
            PROTOCOL,
            &ProductionControl::new(&memory, &token, &token)
        )
        .is_err());
        assert_eq!(memory.used(), 0);
    }
}
