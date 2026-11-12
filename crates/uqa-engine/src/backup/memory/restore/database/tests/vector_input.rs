//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn repository_vector_input_preserves_current_windows_all_native_kinds_and_sql_writes() {
    for method in ["", "ivf", "hnsw", "diskann"] {
        let directory = tempfile::tempdir().unwrap();
        let repository = repository(&directory.path().join("repository"));
        let source = Engine::new();
        source.sql("CREATE TABLE items(id integer PRIMARY KEY, embedding vector(2)); INSERT INTO items VALUES(1,ARRAY[1.0,0.0]),(2,ARRAY[0.0,1.0]),(3,ARRAY[-1.0,0.0])", &[]).unwrap();
        if !method.is_empty() {
            source
                .sql(
                    &format!("CREATE INDEX items_vector ON items USING {method}(embedding)"),
                    &[],
                )
                .unwrap();
        }
        let query = "SELECT id, _score FROM items WHERE knn_match(embedding, ARRAY[1.0,0.0], 3) ORDER BY id";
        let expected_rows = source.sql(query, &[]).unwrap().rows;
        assert_eq!(expected_rows.len(), 3);
        let expected_image = image(&source);
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
                    unit: DecodedMemoryTableUnit::Vector(field),
                    ..
                } = unit.address
                {
                    let count = counts
                        .entry(field.to_string())
                        .or_insert((0u64, 0u64, 0u64));
                    count.0 += 1;
                    count.1 += maximum as u64;
                    count.2 = unit.decoded_bytes;
                }
            }
        };
        assert_eq!(counts.len(), 1, "{method}");
        for (calls, allowance, consumed) in counts.values() {
            assert!(*calls > 1 && *consumed > 17);
            assert!(
                consumed <= allowance,
                "{method}: consumed {consumed} with {allowance} admitted bytes"
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
        assert_eq!(image(&restored), expected_image, "{method}");
        assert_eq!(
            restored.sql(query, &[]).unwrap().rows,
            expected_rows,
            "{method}"
        );
        restored.sql("BEGIN; UPDATE items SET embedding=ARRAY[-1.0,0.0]; ROLLBACK; INSERT INTO items VALUES(4,ARRAY[0.5,0.5]); DELETE FROM items WHERE id=2", &[]).unwrap();
        let rows = restored.sql(query, &[]).unwrap().rows;
        assert_eq!(
            rows.iter().map(|row| row["id"].clone()).collect::<Vec<_>>(),
            vec![Value::Int(1), Value::Int(3), Value::Int(4)],
            "{method}"
        );
        drop(restored);
        assert_eq!(control.memory().used(), 0, "{method}");
    }
}
