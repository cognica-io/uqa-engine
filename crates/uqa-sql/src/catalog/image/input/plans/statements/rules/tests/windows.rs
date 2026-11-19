//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::catalog::image::input::window_tests::{assert_retirement, read_windowed, WINDOWS};

#[test]
fn native_rule_windows_preserve_actions_conditions_and_nested_destination_edits() {
    let source = fixture();
    let bytes = encode(&source);
    for (width, steps) in WINDOWS {
        let memory = MemoryBudget::new(64 << 20);
        let mutations = MemoryBudget::new(16 << 20);
        let token = CancellationToken::new();
        let mut value = read_windowed(
            RuleCatalog::image_input(&memory, &mutations, &token).unwrap(),
            &bytes,
            width,
            steps,
        );
        assert_eq!(encode(&value), bytes);
        assert_eq!(
            serde_json::to_value(value.iter().collect::<Vec<_>>()).unwrap(),
            serde_json::to_value(source.iter().collect::<Vec<_>>()).unwrap()
        );
        let retained = value.clone();
        let relation = RelationIdentity::new("public", "items");
        let mut replacement = value.get(&relation).unwrap().get("watch").unwrap().clone();
        replacement.enabled = EventEnableMode::Disabled;
        let full = memory.reserve(memory.available()).unwrap();
        token.cancel();
        value
            .insert_event(relation.clone(), "watch".into(), replacement)
            .unwrap();
        assert_eq!(
            value.get(&relation).unwrap().get("watch").unwrap().enabled,
            EventEnableMode::Disabled
        );
        assert_eq!(encode(&retained), bytes);
        drop((value, full));
        assert_eq!(mutations.used(), 0);
        assert!(memory.used() > 0);
        drop(retained);
        assert_eq!(memory.used(), 0);
    }
    assert_retirement(&bytes, |memory, token| {
        RuleCatalog::image_input(memory, memory, token)
    });
}

#[test]
fn native_rule_windows_retain_original_identity_claims_across_relations() {
    let mut source = fixture();
    let mut duplicate = source
        .get(&RelationIdentity::new("public", "items"))
        .unwrap()
        .get("watch")
        .unwrap()
        .clone();
    duplicate.definition.table = "public.other".into();
    source
        .insert_event(
            RelationIdentity::new("public", "other"),
            "watch".into(),
            duplicate,
        )
        .unwrap();
    let bytes = encode(&source);
    let memory = MemoryBudget::new(64 << 20);
    let token = CancellationToken::new();
    let mut input = RuleCatalog::image_input(&memory, &memory, &token).unwrap();
    let mut reader = bytes.as_slice();
    loop {
        match input.advance(
            &mut reader,
            uqa_core::image_io::ImageDecodeWindow { bytes: 1, steps: 3 },
        ) {
            Ok(None) => {}
            Ok(Some(_)) => panic!("duplicate rule catalog identity accepted"),
            Err(_) => break,
        }
    }
    assert_eq!(memory.used(), 0);
    let before = reader.len();
    assert!(input
        .advance(
            &mut reader,
            uqa_core::image_io::ImageDecodeWindow { bytes: 1, steps: 1 }
        )
        .is_err());
    assert_eq!(reader.len(), before);
}

#[test]
fn native_rule_windows_keep_original_and_bound_conditions_on_small_stacks() {
    let bytes = independent(10_000);
    std::thread::Builder::new()
        .stack_size(128 << 10)
        .spawn(move || {
            let memory = MemoryBudget::new(128 << 20);
            let token = CancellationToken::new();
            let value = read_windowed(
                RuleCatalog::image_input(&memory, &memory, &token).unwrap(),
                &bytes,
                17,
                3,
            );
            assert_eq!(encode(&value), bytes);
            let full = memory.reserve(memory.available()).unwrap();
            assert_eq!(allocation_counter::measure(|| drop(value)).count_total, 0);
            drop(full);
            assert_eq!(memory.used(), 0);
        })
        .unwrap()
        .join()
        .unwrap();
}
