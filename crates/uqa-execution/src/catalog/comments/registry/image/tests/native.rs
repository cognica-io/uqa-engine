//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn native_comment_edits_preserve_original_input_leases() {
    let key = address(7, 0, 1259);
    let removed = address(8, 0, 1259);
    let expected = wire(&[(key, b"original"), (removed, b"removed")]);
    let input = MemoryBudget::new(1 << 20);
    let mutations = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let mut decoded = CommentRegistry::read_image_with_mutation_budget(
        &mut expected.as_slice(),
        &input,
        &mutations,
        &token,
    )
    .unwrap();
    let selected = decoded.clone();
    let original = selected.get(&key).unwrap().as_ptr();
    let occupied = input.reserve(input.available()).unwrap();
    token.cancel();
    decoded
        .insert(key, "changed", RelationPersistence::Permanent)
        .unwrap();
    decoded
        .insert(
            address(9, 0, 1259),
            "private",
            RelationPersistence::Temporary,
        )
        .unwrap();
    assert!(decoded.remove(&removed).unwrap());
    let output = MemoryBudget::new(1 << 20);
    let output_token = CancellationToken::new();
    let control = ProductionControl::new(&output, &output_token, &output_token);
    let before = bytes(&decoded.capture(&control).unwrap());
    let denied = mutations.reserve(mutations.available()).unwrap();
    assert!(decoded
        .insert(key, "denied", RelationPersistence::Permanent)
        .is_err());
    assert_eq!(bytes(&decoded.capture(&control).unwrap()), before);
    assert_eq!(selected.get(&key).unwrap().as_ptr(), original);
    assert_eq!(bytes(&selected.capture(&control).unwrap()), expected);
    drop((decoded, denied));
    assert_eq!(mutations.used(), 0);
    assert!(input.used() > occupied.bytes());
    drop(selected);
    assert_eq!(input.used(), occupied.bytes());
    drop(occupied);
    assert_eq!(input.used(), 0);
}
