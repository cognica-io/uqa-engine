//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn complete_repository_input_rejects_missing_foreign_key_tables() {
    let directory = tempfile::tempdir().unwrap();
    let repository = repository(&directory.path().join("repository"));
    let source = Engine::new();
    source.sql("CREATE TABLE parent(id integer PRIMARY KEY); CREATE TABLE child(id integer REFERENCES parent(id))", &[]).unwrap();
    source
        .storage
        .tables
        .write()
        .remove(&uqa_core::RelationIdentity::new("public", "parent"))
        .unwrap()
        .unwrap();
    assert!(source
        .durable
        .catalog_indexes
        .write()
        .remove(&uqa_core::RelationIdentity::new("public", "parent_pkey"))
        .unwrap());
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
        panic!("missing foreign-key table reached private activation");
    };
    assert!(error.to_string().contains("native constraint"), "{error}");
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn complete_repository_input_rejects_broken_constraint_columns_and_index_targets() {
    for corruption in [
        "index",
        "target_column",
        "local_column",
        "type",
        "check",
        "key_column",
    ] {
        let directory = tempfile::tempdir().unwrap();
        let repository = repository(&directory.path().join("repository"));
        let source = Engine::new();
        source.sql("CREATE TABLE parent(id integer PRIMARY KEY); CREATE TABLE child(id integer, extra integer CHECK(extra>0), FOREIGN KEY(id) REFERENCES parent(id))", &[]).unwrap();
        {
            let tables = source.storage.tables.read();
            let child = tables
                .get(&uqa_core::RelationIdentity::new("public", "child"))
                .unwrap();
            match corruption {
                "index" => child.foreign_keys.write()[0].referenced_index = Some([255; 16]),
                "target_column" => child.foreign_keys.write()[0].ref_columns[0] = "absent".into(),
                "local_column" => child.foreign_keys.write()[0].local_columns[0] = "absent".into(),
                "type" => child
                    .columns
                    .write()
                    .edit(|columns| {
                        columns[0].ty = uqa_sql::ColumnType::Boolean;
                        Ok(())
                    })
                    .unwrap(),
                "check" => child
                    .columns
                    .write()
                    .edit(|columns| {
                        columns[1].check = Some(uqa_sql::ast::Expr::Column("absent".into()));
                        Ok(())
                    })
                    .unwrap(),
                "key_column" => {
                    let parent = tables
                        .get(&uqa_core::RelationIdentity::new("public", "parent"))
                        .unwrap();
                    parent.key_constraints.write()[0].columns[0] = "absent".into();
                    let mut indexes = source.durable.catalog_indexes.write();
                    let mut index = indexes
                        .get(&uqa_core::RelationIdentity::new("public", "parent_pkey"))
                        .unwrap()
                        .clone();
                    index.columns_json = r#"["absent"]"#.into();
                    indexes.replace(index).unwrap();
                }
                _ => unreachable!(),
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
            panic!("broken constraint reached private activation: {corruption}");
        };
        let expected = if corruption == "key_column" {
            "invalid native index reference: missing live column"
        } else {
            "native constraint"
        };
        assert!(
            error.to_string().contains(expected),
            "{corruption}: {error}"
        );
        assert_eq!(control.memory().used(), 0);
    }
}

#[test]
fn complete_repository_input_preserves_foreign_keys_checks_and_partition_rows() {
    let directory = tempfile::tempdir().unwrap();
    let repository = repository(&directory.path().join("repository"));
    let source = Engine::new();
    source.sql("CREATE SCHEMA saved; CREATE TABLE parent(id integer, kind integer, PRIMARY KEY(id,kind)); CREATE TABLE child(x smallint, y integer, FOREIGN KEY(x,y) REFERENCES parent(kind,id) ON DELETE SET NULL(x,x)); CREATE TABLE repeated(x integer, FOREIGN KEY(x,x) REFERENCES parent(id,kind)); INSERT INTO parent VALUES(5,5); INSERT INTO child VALUES(5,5); INSERT INTO repeated VALUES(5); CREATE TABLE checked(v integer); INSERT INTO checked VALUES(-1); ALTER TABLE checked ADD CONSTRAINT positive CHECK(v>0) NOT VALID; CREATE TABLE parts(id integer PRIMARY KEY) PARTITION BY RANGE(id); CREATE TABLE left_part PARTITION OF parts FOR VALUES FROM(0) TO(10) PARTITION BY RANGE(id); CREATE TABLE leaf PARTITION OF left_part FOR VALUES FROM(0) TO(10); CREATE TABLE refs(id integer REFERENCES parts(id)); INSERT INTO parts VALUES(2); INSERT INTO refs VALUES(2); ALTER INDEX parent_pkey RENAME TO original_key; ALTER TABLE parent RENAME TO original_parent; ALTER TABLE original_parent SET SCHEMA saved", &[]).unwrap();
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
    let restored = state
        .into_private_engine(MemoryBudget::new(32 << 20), &control)
        .unwrap();
    assert_eq!(image(&restored), expected);
    assert_eq!(
        restored.sql("SELECT v FROM checked", &[]).unwrap().rows[0]["v"],
        Value::Int(-1)
    );
    assert!(restored.sql("INSERT INTO checked VALUES(-2)", &[]).is_err());
    assert!(restored.sql("INSERT INTO refs VALUES(7)", &[]).is_err());
    assert!(restored.sql("INSERT INTO repeated VALUES(7)", &[]).is_err());
    restored.sql("INSERT INTO saved.original_parent VALUES(7,7); INSERT INTO child VALUES(7,7); INSERT INTO repeated VALUES(7); INSERT INTO parts VALUES(3); INSERT INTO refs VALUES(3); INSERT INTO checked VALUES(1)", &[]).unwrap();
    restored.sql("DELETE FROM child; DELETE FROM repeated; DELETE FROM saved.original_parent; INSERT INTO saved.original_parent VALUES(9,9); INSERT INTO child VALUES(9,9); DELETE FROM saved.original_parent", &[]).unwrap();
    assert_eq!(
        restored.sql("SELECT x,y FROM child", &[]).unwrap().rows[0]["x"],
        Value::Null
    );
    drop(restored);
    assert_eq!(control.memory().used(), 0);
}
