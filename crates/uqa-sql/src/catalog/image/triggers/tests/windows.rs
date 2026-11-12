//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::catalog::image::input::window_tests::{assert_retirement, read_windowed, WINDOWS};
use uqa_core::image_io::ImageDecodeWindow;

#[test]
fn trigger_input_bounds_native_conditions_and_preserves_nested_mutation_owners() {
    let mut trigger = native();
    trigger.definition.arguments.push("한🙂".repeat(1025));
    trigger.definition.when = Some(Expr::IsNull {
        expr: Box::new(Expr::Literal(uqa_core::Value::Str("한🙂".repeat(1025)))),
        negated: true,
    });
    let expected = encode(&catalog(trigger.clone()), 17);
    for (width, steps) in WINDOWS {
        let memory = MemoryBudget::new(16 << 20);
        let mutations = MemoryBudget::new(16 << 20);
        let token = CancellationToken::new();
        let mut restored = read_windowed(
            TriggerCatalog::image_input(&memory, &mutations, &token).unwrap(),
            &expected,
            width,
            steps,
        );
        assert_eq!(encode(&restored, 31), expected);
        let retained = restored.clone();
        let mut replacement = trigger.clone();
        replacement.enabled = EventEnableMode::Disabled;
        let full = memory.reserve(memory.available()).unwrap();
        token.cancel();
        restored
            .insert_event(RelationIdentity::new("s", "t"), "watch".into(), replacement)
            .unwrap();
        assert_eq!(encode(&retained, 31), expected);
        drop((restored, full));
        assert_eq!(mutations.used(), 0);
        assert!(memory.used() > 0);
        drop(retained);
        assert_eq!(memory.used(), 0);
    }
    assert_retirement(&expected, |memory, token| {
        TriggerCatalog::image_input(memory, memory, token)
    });
}

#[test]
fn trigger_input_keeps_identity_claims_across_partial_relation_windows() {
    let mut source = catalog(native());
    let first = encode(&source, 17);
    let mut second = native();
    second.definition.table = "s.u".into();
    source
        .insert_event(RelationIdentity::new("s", "u"), "watch".into(), second)
        .unwrap();
    let expected = encode(&source, 17);
    let memory = MemoryBudget::new(16 << 20);
    let token = CancellationToken::new();
    let mut input = TriggerCatalog::image_input(&memory, &memory, &token).unwrap();
    let mut reader = std::io::Cursor::new(expected);
    let window = ImageDecodeWindow { bytes: 1, steps: 3 };
    let mut rejected = false;
    for _ in 0..reader.get_ref().len() * 8 {
        match input.advance(&mut reader, window) {
            Ok(None) => (),
            Ok(Some(_)) => panic!("duplicate trigger identity accepted"),
            Err(_) => {
                rejected = true;
                break;
            }
        }
    }
    assert!(rejected);
    assert!(reader.position() > first.len() as u64);
    assert_eq!(memory.used(), 0);
    let position = reader.position();
    assert!(input.advance(&mut reader, window).is_err());
    assert_eq!(reader.position(), position);
}
