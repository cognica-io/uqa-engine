//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

fn analyzer_source() -> Engine {
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
    let mut words: Vec<_> = (0..64)
        .map(|index| format!("word{index}{}", "尾".repeat(32)))
        .collect();
    words.push("skip".into());
    let config = serde_json::json!({"tokenizer":{"type":"whitespace"},"token_filters":[{"type":"stop","language":"","custom_words":words}]}).to_string();
    source
        .register_named_analyzer("historical", &config)
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
    source.register_named_analyzer("wide", &config).unwrap();
    source
        .sql("INSERT INTO items VALUES('Cats skip DOGS')", &[])
        .unwrap();
    source
}

fn component(address: &DecodedMemoryImageAddress) -> Option<usize> {
    match address {
        DecodedMemoryImageAddress::FieldAnalyzer { .. } => Some(0),
        DecodedMemoryImageAddress::Resource { kind: 4, .. } => Some(1),
        _ => None,
    }
}

fn progress(input: &Input<'_>) -> Option<(usize, (String, String), u64)> {
    let image = input.image.as_ref()?;
    let unit = image.current_unit()?;
    let key = match unit.address {
        DecodedMemoryImageAddress::FieldAnalyzer { table, field } => {
            (table.to_string(), field.to_string())
        }
        DecodedMemoryImageAddress::Resource { kind: 4, name } => (name.to_string(), String::new()),
        _ => return None,
    };
    Some((component(unit.address)?, key, image.current_read_bytes()?))
}

#[test]
fn repository_analyzer_and_field_input_obey_current_windows_after_source_retirement() {
    let directory = tempfile::tempdir().unwrap();
    let repository = repository(&directory.path().join("repository"));
    let source = analyzer_source();
    let expected = image(&source);
    let root = publish(&source, &repository);
    drop(source);
    let control = StorageReadControl::with_limit(32 << 20);
    let mut input = input(&repository, root, &control);
    let mut calls = [0; 2];
    let mut iteration = 0;
    let state = loop {
        let before = progress(&input);
        let maximum = if iteration % 2 == 0 { 1 } else { 17 };
        iteration += 1;
        if let Some(state) = input.advance(maximum).unwrap() {
            break state;
        }
        if let Some((index, key, bytes)) = progress(&input) {
            calls[index] += 1;
            let previous = before
                .filter(|(previous, previous_key, _)| *previous == index && *previous_key == key)
                .map_or(0, |(_, _, bytes)| bytes);
            assert!(bytes - previous <= maximum as u64);
        }
    };
    assert!(
        calls.iter().all(|&count| count > 1),
        "analyzer/field input completed in one call: {calls:?}"
    );
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
    assert_eq!(index.analyze("Cats skip DOGS").unwrap(), ["Cats", "DOGS"]);
    assert_eq!(
        search.analyze("Cats skip DOGS").unwrap(),
        ["Cats skip DOGS"]
    );
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
    drop((table, index, search, restored));
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn repository_analyzer_and_field_interruption_retires_private_catalogs() {
    let directory = tempfile::tempdir().unwrap();
    let repository = repository(&directory.path().join("repository"));
    let source = analyzer_source();
    let root = publish(&source, &repository);
    drop(source);
    for selected in 0..2 {
        for mode in 0..4 {
            let control = StorageReadControl::with_limit(32 << 20);
            let mut input = input(&repository, root, &control);
            let mut calls = 0;
            loop {
                assert!(input.advance(1).unwrap().is_none());
                if input
                    .image
                    .as_ref()
                    .unwrap()
                    .current_unit()
                    .is_some_and(|unit| component(unit.address) == Some(selected))
                {
                    calls += 1;
                    if calls == 30 {
                        break;
                    }
                }
            }
            let held = if mode == 1 {
                Some(
                    control
                        .memory()
                        .reserve(control.memory().available())
                        .unwrap(),
                )
            } else {
                None
            };
            if mode == 0 {
                control.cancellation().cancel();
            }
            if mode != 2 {
                while let Ok(state) = input.advance(usize::from(mode != 3)) {
                    assert!(state.is_none());
                }
                assert!(input.candidate.is_none() && input.image.is_none());
                assert!(matches!(input.advance(1), Err(BackupError::Finished)));
            }
            drop((input, held));
            assert_eq!(
                control.memory().used(),
                0,
                "component {selected}, interruption {mode}"
            );
        }
    }
}
