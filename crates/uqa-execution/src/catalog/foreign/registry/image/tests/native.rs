//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn native_foreign_edits_keep_paired_original_input_owners() {
    let expected = image();
    let input = StorageReadControl::with_limit(8 << 20);
    let mutations = MemoryBudget::new(8 << 20);
    let mut decoded = ForeignCatalog::read_image_with_mutation_budget(
        &mut expected.as_slice(),
        &input,
        &mutations,
    )
    .unwrap();
    let selected = decoded.clone();
    let original = selected.tables().get(&key()).unwrap().server_name.as_ptr();
    let occupied = input.memory().reserve(input.memory().available()).unwrap();
    input.cancellation().cancel();
    let mut changed = table();
    changed.server_name = "replacement".into();
    decoded.replace_table(key(), changed).unwrap();
    let mut security = authority();
    security.acl_revisions.relation = Some([9; 16]);
    decoded.replace_security(&key(), security.clone()).unwrap();
    assert_eq!(
        decoded.tables().get(&key()).unwrap().server_name,
        "replacement"
    );
    assert_eq!(decoded.security().get(&key()), Some(&security));
    let output = MemoryBudget::new(8 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&output, &token, &token);
    let before = collect(&decoded.capture(&control).unwrap(), 11);
    let denied = mutations.reserve(mutations.available()).unwrap();
    assert!(decoded.insert(key(), table(), authority()).is_err());
    assert_eq!(collect(&decoded.capture(&control).unwrap(), 13), before);
    assert_eq!(
        selected.tables().get(&key()).unwrap().server_name.as_ptr(),
        original
    );
    assert_eq!(collect(&selected.capture(&control).unwrap(), 17), expected);
    drop(denied);
    assert!(decoded.remove(&key()).unwrap());
    assert!(decoded.tables().is_empty());
    assert!(decoded.security().is_empty());
    drop(decoded);
    assert_eq!(mutations.used(), 0);
    assert!(input.memory().used() > occupied.bytes());
    drop(selected);
    assert_eq!(input.memory().used(), occupied.bytes());
    drop(occupied);
    assert_eq!(input.memory().used(), 0);
}
