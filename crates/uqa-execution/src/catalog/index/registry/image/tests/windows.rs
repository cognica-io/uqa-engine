//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::catalog::image_input_tests::{assert_retirement, read_windowed, WINDOWS};

#[test]
fn native_index_input_bounds_fields_and_key_copies_and_preserves_mutation_ownership() {
    let suffix = "한🙂".repeat(1025);
    let mut rows = [
        row(&format!("a{suffix}"), Some("{ \"x\": 1.00, \"x\": 2 }")),
        row(&format!("b{suffix}"), None),
    ];
    for row in &mut rows {
        row.relation.schema.clone_from(&suffix);
    }
    let expected = wire(&rows);
    for (width, steps) in WINDOWS {
        let memory = MemoryBudget::new(1 << 20);
        let mutations = MemoryBudget::new(1 << 20);
        let token = CancellationToken::new();
        let mut restored = read_windowed(
            CatalogIndexes::image_input(&memory, &mutations, &token).unwrap(),
            &expected,
            width,
            steps,
        );
        let control = StorageReadControl::with_limit(1 << 20);
        assert_eq!(bytes(&restored.capture(&control).unwrap()), expected);
        let retained = restored.clone();
        let pointer = retained
            .get(&rows[0].relation)
            .unwrap()
            .relation
            .name
            .as_ptr();
        let full = memory.reserve(memory.available()).unwrap();
        token.cancel();
        let mut replacement = rows[0].clone();
        replacement.definition_json = Some("changed".into());
        restored.replace(replacement).unwrap();
        restored.remove(&rows[1].relation).unwrap();
        assert_eq!(
            retained
                .get(&rows[0].relation)
                .unwrap()
                .relation
                .name
                .as_ptr(),
            pointer
        );
        assert_eq!(bytes(&retained.capture(&control).unwrap()), expected);
        drop((restored, full));
        assert_eq!(mutations.used(), 0);
        assert!(memory.used() > 0);
        drop(retained);
        assert_eq!(memory.used(), 0);
    }
    assert_retirement(&expected, CatalogIndexes::image_input);
}
