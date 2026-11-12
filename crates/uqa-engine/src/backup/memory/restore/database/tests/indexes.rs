//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn complete_repository_input_rejects_missing_index_tables() {
    let directory = tempfile::tempdir().unwrap();
    let repository = repository(&directory.path().join("repository"));
    let source = Engine::new();
    source
        .sql(
            "CREATE TABLE items(value integer); CREATE INDEX saved ON items(value)",
            &[],
        )
        .unwrap();
    source
        .storage
        .tables
        .write()
        .remove(&uqa_core::RelationIdentity::new("public", "items"))
        .unwrap()
        .unwrap();
    let root = publish(&source, &repository);
    drop(source);
    let control = StorageReadControl::with_limit(32 << 20);
    let mut input = input(&repository, root, &control);
    let state = loop {
        if let Some(state) = input.advance(1024).unwrap() {
            break state;
        }
    };
    drop(input);
    let Err(error) = state.into_private_engine(MemoryBudget::new(32 << 20), &control) else {
        panic!("missing index table reached private activation");
    };
    assert!(error.to_string().contains("native index"), "{error}");
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn complete_repository_input_rejects_broken_index_constraint_identities() {
    for corruption in [
        "table",
        "constraint",
        "index_oid",
        "columns",
        "parent",
        "missing",
    ] {
        let directory = tempfile::tempdir().unwrap();
        let repository = repository(&directory.path().join("repository"));
        let source = Engine::new();
        source.sql("CREATE TABLE items(id integer PRIMARY KEY, extra integer); INSERT INTO items VALUES(1,7)", &[]).unwrap();
        {
            let mut indexes = source.durable.catalog_indexes.write();
            let name = uqa_core::RelationIdentity::new("public", "items_pkey");
            let mut row = indexes.get(&name).unwrap().clone();
            if corruption == "missing" {
                indexes.remove(&name).unwrap();
            } else {
                let mut definition = uqa_sql::catalog::index::stored::index_definition(
                    row.definition_json.as_deref(),
                )
                .unwrap();
                match corruption {
                    "table" => definition.catalog.as_mut().unwrap().table_object_id = [255; 16],
                    "constraint" => definition.relationships.owning_constraint = Some([255; 16]),
                    "index_oid" => definition.catalog.as_mut().unwrap().identity.oid += 1,
                    "columns" => row.columns_json = r#"["extra"]"#.into(),
                    "parent" => definition.relationships.parent_index = Some([255; 16]),
                    _ => unreachable!(),
                }
                row.definition_json = Some(serde_json::to_string(&definition).unwrap());
                indexes.replace(row).unwrap();
            }
        }
        let root = publish(&source, &repository);
        drop(source);
        let control = StorageReadControl::with_limit(32 << 20);
        let mut input = input(&repository, root, &control);
        let state = loop {
            if let Some(state) = input.advance(1024).unwrap() {
                break state;
            }
        };
        drop(input);
        let Err(error) = state.into_private_engine(MemoryBudget::new(32 << 20), &control) else {
            panic!("broken index ownership reached private activation: {corruption}");
        };
        assert!(
            error.to_string().contains("native index"),
            "{corruption}: {error}"
        );
        assert_eq!(control.memory().used(), 0);
    }
}

#[test]
fn complete_repository_input_preserves_renamed_constraint_and_partition_indexes() {
    let directory = tempfile::tempdir().unwrap();
    let repository = repository(&directory.path().join("repository"));
    let source = Engine::new();
    source.sql("CREATE SCHEMA saved; CREATE TABLE items(id integer PRIMARY KEY, extra integer); CREATE TABLE refs(id integer REFERENCES items(id)); CREATE TABLE parts(id integer PRIMARY KEY) PARTITION BY RANGE(id); CREATE TABLE left_part PARTITION OF parts FOR VALUES FROM(0) TO(10); CREATE TABLE right_part PARTITION OF parts FOR VALUES FROM(10) TO(20); INSERT INTO items VALUES(1,7); INSERT INTO refs VALUES(1); INSERT INTO parts VALUES(2),(12); ALTER INDEX items_pkey RENAME TO renamed_key; ALTER TABLE items RENAME TO renamed_items; ALTER TABLE renamed_items SET SCHEMA saved", &[]).unwrap();
    let expected = image(&source);
    let root = publish(&source, &repository);
    drop(source);
    let control = StorageReadControl::with_limit(32 << 20);
    let mut input = input(&repository, root, &control);
    let state = loop {
        if let Some(state) = input.advance(1024).unwrap() {
            break state;
        }
    };
    drop(input);
    let engine = state
        .into_private_engine(MemoryBudget::new(32 << 20), &control)
        .unwrap();
    assert_eq!(image(&engine), expected);
    assert!(engine
        .sql("INSERT INTO saved.renamed_items VALUES(1,9)", &[])
        .is_err());
    assert!(engine.sql("INSERT INTO parts VALUES(2)", &[]).is_err());
    assert!(engine.sql("INSERT INTO refs VALUES(9)", &[]).is_err());
    engine.sql("INSERT INTO saved.renamed_items VALUES(3,9); INSERT INTO refs VALUES(3); INSERT INTO parts VALUES(3),(13)", &[]).unwrap();
    assert_eq!(
        engine
            .sql("SELECT id FROM parts ORDER BY id", &[])
            .unwrap()
            .rows
            .into_iter()
            .map(|row| row["id"].clone())
            .collect::<Vec<_>>(),
        vec![Value::Int(2), Value::Int(3), Value::Int(12), Value::Int(13)]
    );
    drop(engine);
    assert_eq!(control.memory().used(), 0);
}
