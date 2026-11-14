//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn repository_text_input_obeys_current_windows_and_preserves_search_and_writes() {
    let directory = tempfile::tempdir().unwrap();
    let repository = repository(&directory.path().join("repository"));
    let source = Engine::new();
    source.sql("CREATE TABLE items(id integer PRIMARY KEY, body text); CREATE INDEX items_body ON items USING gin(body); INSERT INTO items VALUES(1, repeat('한글 alpha ', 1024)), (2, 'beta')", &[]).unwrap();
    let query = "SELECT id, _score FROM items WHERE text_match(body, 'alpha') ORDER BY id";
    let rows = source.sql(query, &[]).unwrap().rows;
    assert_eq!(rows.len(), 1);
    let expected = image(&source);
    let root = publish(&source, &repository);
    drop(source);
    let control = StorageReadControl::with_limit(32 << 20);
    let mut input = input(&repository, root, &control);
    let mut counts = std::collections::BTreeMap::new();
    let mut iteration = 0;
    let state = loop {
        iteration += 1;
        let maximum = if iteration % 2 == 0 { 1 } else { 17 };
        if let Some(state) = input.advance(maximum).unwrap() {
            break state;
        }
        if let Some(unit) = input.image.as_ref().unwrap().current_unit() {
            if let DecodedMemoryImageAddress::Table {
                unit: DecodedMemoryTableUnit::Text(id),
                ..
            } = unit.address
            {
                let count = counts.entry(*id).or_insert((0u64, 0u64, 0u64));
                count.0 += 1;
                count.1 += maximum as u64;
                count.2 = unit.decoded_bytes;
            }
        }
    };
    assert_eq!(counts.len(), 2);
    for (calls, allowed, length) in counts.values() {
        assert!(*calls > 1 && *length > 17);
        assert!(
            *length <= *allowed,
            "text image consumed {length} bytes with {allowed} bytes of call allowance"
        );
    }
    drop(input);
    let restored = state
        .into_private_engine(MemoryBudget::new(32 << 20), &control)
        .unwrap();
    repository
        .remove_backup(root.backup(), &StorageReadControl::with_limit(1 << 20))
        .unwrap();
    drop(repository);
    control.cancellation().cancel();
    assert_eq!(image(&restored), expected);
    assert_eq!(restored.sql(query, &[]).unwrap().rows, rows);
    restored.sql("BEGIN; UPDATE items SET body='discarded'; ROLLBACK; INSERT INTO items VALUES(3,'alpha later'); DELETE FROM items WHERE id=2", &[]).unwrap();
    let actual = restored
        .sql(
            "SELECT id FROM items WHERE text_match(body, 'alpha') ORDER BY id",
            &[],
        )
        .unwrap()
        .rows;
    assert_eq!(
        actual
            .iter()
            .map(|row| row["id"].clone())
            .collect::<Vec<_>>(),
        vec![Value::Int(1), Value::Int(3)]
    );
    drop(restored);
    assert_eq!(control.memory().used(), 0);
}
