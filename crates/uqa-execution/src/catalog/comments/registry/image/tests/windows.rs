//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::catalog::image_input_tests::{assert_retirement, read_windowed, WINDOWS};

#[test]
fn native_comment_input_bounds_utf8_validation_and_preserves_mutation_ownership() {
    let first = address(7, 0, 1259);
    let second = address(7, 3, 1259);
    let value = "한🙂".repeat(2049);
    let expected = wire(&[(first, value.as_bytes()), (second, b"column")]);
    for (width, steps) in WINDOWS {
        let memory = MemoryBudget::new(1 << 20);
        let mutations = MemoryBudget::new(1 << 20);
        let token = CancellationToken::new();
        let mut restored = read_windowed(
            CommentRegistry::image_input(&memory, &mutations, &token).unwrap(),
            &expected,
            width,
            steps,
        );
        assert_eq!(restored.get(&first), Some(&value));
        let retained = restored.clone();
        let pointer = retained.get(&first).unwrap().as_ptr();
        let full = memory.reserve(memory.available()).unwrap();
        token.cancel();
        restored
            .insert(first, "changed", RelationPersistence::Permanent)
            .unwrap();
        assert!(restored.remove(&second).unwrap());
        assert_eq!(retained.get(&first).unwrap().as_ptr(), pointer);
        let output = MemoryBudget::new(1 << 20);
        let output_token = CancellationToken::new();
        let control = ProductionControl::new(&output, &output_token, &output_token);
        assert_eq!(bytes(&retained.capture(&control).unwrap()), expected);
        drop((restored, full));
        assert_eq!(mutations.used(), 0);
        assert!(memory.used() > 0);
        drop(retained);
        assert_eq!(memory.used(), 0);
    }
    assert_retirement(&expected, CommentRegistry::image_input);
}
