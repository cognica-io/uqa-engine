//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn native_index_catalog_edits_preserve_original_input_leases() {
    let expected = wire(&[row("a", Some("original")), row("b", None)]);
    let input = MemoryBudget::new(1 << 20);
    let mutations = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let mut decoded = CatalogIndexes::read_image_with_mutation_budget(
        &mut expected.as_slice(),
        &input,
        &mutations,
        &token,
    )
    .unwrap();
    let selected = decoded.clone();
    let key = row("a", None).relation;
    let original = selected
        .get(&key)
        .unwrap()
        .definition_json
        .as_ref()
        .unwrap()
        .as_ptr();
    let occupied = input.reserve(input.available()).unwrap();
    token.cancel();
    decoded.replace(row("a", Some("changed"))).unwrap();
    decoded
        .insert(row("new", None), RelationPersistence::Permanent)
        .unwrap();
    decoded
        .insert(row("private", None), RelationPersistence::Temporary)
        .unwrap();
    decoded.remove(&row("b", None).relation).unwrap();
    assert_eq!(
        decoded.get(&key).unwrap().definition_json.as_deref(),
        Some("changed")
    );
    let output = StorageReadControl::with_limit(1 << 20);
    let before = bytes(&decoded.capture(&output).unwrap());
    let denied = mutations.reserve(mutations.available()).unwrap();
    assert!(decoded.replace(row("a", Some("denied"))).is_err());
    assert_eq!(bytes(&decoded.capture(&output).unwrap()), before);
    assert_eq!(
        selected
            .get(&key)
            .unwrap()
            .definition_json
            .as_ref()
            .unwrap()
            .as_ptr(),
        original
    );
    assert_eq!(bytes(&selected.capture(&output).unwrap()), expected);
    drop((decoded, denied));
    assert_eq!(mutations.used(), 0);
    assert!(input.used() > occupied.bytes());
    drop(selected);
    assert_eq!(input.used(), occupied.bytes());
    drop(occupied);
    assert_eq!(input.used(), 0);
}
