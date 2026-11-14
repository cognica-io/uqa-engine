//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::{catalog_role::RoleIdentity, memory::MemoryBudget, CancellationToken};

fn fixture() -> String {
    serde_json::to_string(&StoredSecurity {
        builtin_routine_acl_format: 1,
        entry: BuiltinRoutineSecurity {
            revision: [1; 16],
            execute_acl: vec![crate::ast::ObjectAclEntry {
                role: None,
                grantor: RoleIdentity::BOOTSTRAP,
                grant_option: false,
            }],
        },
    })
    .unwrap()
}

fn equivalent(text: &str) {
    let memory = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let expected = serde_json::from_str::<StoredSecurity>(text);
    let actual = StoredSecurity::from_stored_json_with_control(
        text,
        &ProductionControl::new(&memory, &token, &token),
    );
    assert_eq!(
        actual.is_ok(),
        expected.is_ok(),
        "{text}: {actual:?}, {expected:?}"
    );
    if let (Ok(actual), Ok(expected)) = (actual, expected) {
        assert_eq!(
            actual.builtin_routine_acl_format,
            expected.builtin_routine_acl_format
        );
        assert_eq!(actual.entry, expected.entry);
    }
    assert_eq!(memory.used(), 0);
}

#[test]
fn stored_builtin_authority_matches_direct_serde_fields_and_native_acl_shapes() {
    let text = fixture();
    equivalent(&text);
    for (end, _) in text.char_indices() {
        equivalent(&text[..end]);
    }
    equivalent(&format!("{text} []"));
    let original: serde_json::Value = serde_json::from_str(&text).unwrap();
    for path in [
        vec![],
        vec!["entry"],
        vec!["entry", "execute_acl", "0"],
        vec!["entry", "execute_acl", "0", "grantor"],
    ] {
        let mut value = original.clone();
        let mut field = &mut value;
        for item in &path {
            field = if *item == "0" {
                &mut field[0]
            } else {
                &mut field[*item]
            };
        }
        let keys: Vec<_> = field.as_object().unwrap().keys().cloned().collect();
        for key in keys {
            let removed = field.as_object_mut().unwrap().remove(&key).unwrap();
            let missing = field.to_string();
            // Replace the same nested object in a complete independent fixture.
            let mut changed = original.clone();
            let mut target = &mut changed;
            for item in &path {
                target = if *item == "0" {
                    &mut target[0]
                } else {
                    &mut target[*item]
                };
            }
            *target = serde_json::from_str(&missing).unwrap();
            equivalent(&changed.to_string());
            field[key] = removed;
        }
        field["unknown"] = serde_json::json!({"ignored":[null,1,"한글"]});
        equivalent(&value.to_string());
    }
    let mut positional = original.clone();
    positional["entry"]["execute_acl"][0] =
        serde_json::json!([null, [10, RoleIdentity::BOOTSTRAP.object_id], false]);
    let entry = &positional["entry"];
    let positional = serde_json::json!([1, [entry["revision"], entry["execute_acl"]]]).to_string();
    equivalent(&positional);
    for format in ["-0", "1.0", "1e0", "4294967295", "4294967296", "null"] {
        equivalent(&text.replacen(
            "\"builtin_routine_acl_format\":1",
            &format!("\"builtin_routine_acl_format\":{format}"),
            1,
        ));
    }
    equivalent(&text.replacen("\"entry\":", "\"entry\":null,\"entry\":", 1));
    equivalent(&text.replacen("\"role\":null", "\"role\":null,\"role\":null", 1));
    for depth in [126, 127, 128, 256] {
        let ignored = format!("{}0{}", "[".repeat(depth), "]".repeat(depth));
        equivalent(&text.replacen(
            "\"grant_option\":false",
            &format!("\"grant_option\":false,\"ignored\":{ignored}"),
            1,
        ));
    }
}

#[test]
fn stored_builtin_authority_adopts_original_buffers_and_independent_mutation_allowance() {
    let memory = MemoryBudget::new(1 << 20);
    let mutations = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&memory, &token, &token);
    let decoded = StoredSecurity::from_stored_json_with_control(&fixture(), &control)
        .unwrap()
        .into_budgeted()
        .unwrap();
    let pointer = decoded.entry.execute_acl.as_ptr();
    let (stored, lease) = decoded.into_parts();
    let mut registry = BuiltinRoutineSecurities::with_budget(&memory);
    insert_stored_entry(&mut registry, 870, Budgeted::new(stored.entry, lease)).unwrap();
    assert_eq!(registry[&870].execute_acl.as_ptr(), pointer);
    let original = registry.clone();
    let mut registry = registry.into_mutation_budget(&mutations);
    let occupied = memory.reserve(memory.available()).unwrap();
    token.cancel();
    registry
        .insert(
            870,
            BuiltinRoutineSecurity {
                revision: [2; 16],
                execute_acl: Vec::new(),
            },
        )
        .unwrap();
    assert_eq!(original[&870].revision, [1; 16]);
    assert_eq!(registry[&870].revision, [2; 16]);
    drop((original, registry));
    assert_eq!(memory.used(), occupied.bytes());
    drop(occupied);
    assert_eq!(memory.used(), 0);
    assert_eq!(mutations.used(), 0);
}
