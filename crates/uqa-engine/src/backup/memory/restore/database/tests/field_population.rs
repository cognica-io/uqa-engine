//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

fn source_with_historical_fields() -> Engine {
    let source = Engine::new();
    source
        .create_table(
            "items",
            uqa_analysis::standard_analyzer("english"),
            vec!["body".into()],
        )
        .unwrap();
    source
        .sql("ALTER TABLE items ADD COLUMN body text", &[])
        .unwrap();
    source
        .register_named_analyzer("historical", r#"{"tokenizer":{"type":"whitespace"}}"#)
        .unwrap();
    source
        .set_table_field_analyzer("items", "body", "historical", "index")
        .unwrap();
    source
        .register_named_analyzer("historical", r#"{"tokenizer":{"type":"keyword"}}"#)
        .unwrap();
    source
        .set_table_field_analyzer("items", "body", "historical", "search")
        .unwrap();
    source
        .sql("INSERT INTO items VALUES('Cats DOGS')", &[])
        .unwrap();
    source
}

#[test]
fn repository_field_population_keeps_historical_revisions_after_source_and_archive_retirement() {
    let directory = tempfile::tempdir().unwrap();
    let repository = repository(&directory.path().join("repository"));
    let source = source_with_historical_fields();
    let expected = image(&source);
    let root = publish(&source, &repository);
    drop(source);
    let control = StorageReadControl::with_limit(32 << 20);
    let mut input = input(&repository, root, &control);
    let state = loop {
        if let Some(state) = input.advance(1).unwrap() {
            break state;
        }
    };
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
    let table = restored
        .storage
        .tables
        .read()
        .values()
        .next()
        .unwrap()
        .clone();
    let index = table
        .inverted_index
        .read()
        .index_analyzer_revision("body")
        .unwrap();
    let search = table
        .inverted_index
        .read()
        .search_analyzer_revision("body")
        .unwrap();
    assert_eq!(index.analyze("Cats DOGS").unwrap(), ["Cats", "DOGS"]);
    assert_eq!(search.analyze("Cats DOGS").unwrap(), ["Cats DOGS"]);
    restored
        .register_named_analyzer("historical", r#"{"tokenizer":{"type":"standard"}}"#)
        .unwrap();
    restored
        .sql(
            "BEGIN; DELETE FROM items; ROLLBACK; INSERT INTO items VALUES('Cats again')",
            &[],
        )
        .unwrap();
    assert_eq!(
        table
            .inverted_index
            .read()
            .get_posting_list("body", "Cats")
            .unwrap()
            .len(),
        2
    );
    assert!(std::sync::Arc::ptr_eq(
        &table
            .inverted_index
            .read()
            .index_analyzer_revision("body")
            .unwrap(),
        &index
    ));
    assert!(std::sync::Arc::ptr_eq(
        &table
            .inverted_index
            .read()
            .search_analyzer_revision("body")
            .unwrap(),
        &search
    ));
    drop((table, restored, index, search));
    assert_eq!(control.memory().used(), 0);
}
