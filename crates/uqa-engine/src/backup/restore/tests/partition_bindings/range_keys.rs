//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

fn reference(kind: &str, ty: &str) -> Vec<&'static str> {
    let text = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/parity/pg18/backup_partition_range_keys_oracle.expected.txt"
    ));
    let prefix = format!("{kind}|{ty}|");
    text.lines()
        .find_map(|line| line.strip_prefix(&prefix))
        .expect("PostgreSQL observation")
        .split(',')
        .collect()
}

fn declare(engine: &Engine, ty: &str, small: &str, middle: &str, large: &str) {
    engine
        .sql(
            &format!("CREATE TABLE parent(id integer, value {ty}) PARTITION BY RANGE(value)"),
            &[],
        )
        .unwrap();
    let error = engine.sql(&format!("CREATE TABLE invalid PARTITION OF parent FOR VALUES FROM ('{large}') TO ('{small}')"), &[]).unwrap_err();
    assert_eq!(error.sqlstate(), Some("42P17"), "{ty}: {error}");
    engine.sql(&format!("CREATE TABLE inside PARTITION OF parent FOR VALUES FROM ('{small}') TO ('{large}'); CREATE TABLE outside PARTITION OF parent DEFAULT; INSERT INTO parent VALUES(1,'{small}'),(2,'{middle}'),(3,'{large}')"), &[]).unwrap();
    let error = engine.sql(&format!("CREATE TABLE invalid PARTITION OF parent FOR VALUES FROM ('{middle}') TO ('{large}')"), &[]).unwrap_err();
    assert_eq!(error.sqlstate(), Some("42P17"), "{ty}: {error}");
}

fn observations(engine: &Engine) -> Vec<String> {
    engine
        .sql(
            "SELECT tableoid::regclass::text AS target FROM parent ORDER BY id",
            &[],
        )
        .unwrap()
        .rows
        .into_iter()
        .map(|row| {
            let Value::Str(value) = &row["target"] else {
                panic!("partition identity")
            };
            value.clone()
        })
        .collect()
}

#[rstest::rstest]
#[case::memory(Provider::Memory)]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn restored_range_keys_preserve_selected_order_and_routing(#[case] provider: Provider) {
    for (ty, small, middle, large) in [
        ("int4range", "[2,3)", "[5,6)", "[10,11)"),
        ("int4multirange", "{[2,3)}", "{[5,6)}", "{[10,11)}"),
        ("numrange", "[2,3)", "[5,6)", "[10,11)"),
        ("nummultirange", "{[2,3)}", "{[5,6)}", "{[10,11)}"),
    ] {
        let directory = tempfile::tempdir().unwrap();
        let repository = repository(&directory.path().join("repository"));
        let source = provider.open(&directory.path().join("source"));
        declare(&source, ty, small, middle, large);
        assert_eq!(observations(&source), reference("range", ty));
        let expected = preserved::definitions(&source);
        let root = capture(&source, &repository, None);
        drop(source);
        let path = directory.path().join("restored");
        let control = StorageReadControl::with_limit(32 << 20);
        let authority = Engine::new();
        let mut job = authority
            .begin_restore(
                &repository,
                root.backup(),
                destination(provider, &path),
                RestoreOptions::new(16 << 20, 8 << 20),
                &control,
            )
            .unwrap();
        prepare(&mut job);
        let restored = job.publish().unwrap();
        assert_eq!(preserved::definitions(&restored), expected);
        assert_eq!(observations(&restored), reference("range", ty));
        drop(job);
        repository
            .remove_backup(root.backup(), &StorageReadControl::with_limit(32 << 20))
            .unwrap();
        restored
            .sql(
                &format!("INSERT INTO parent VALUES(4,'{middle}'),(5,'{large}')"),
                &[],
            )
            .unwrap();
        assert_eq!(
            observations(&restored),
            ["inside", "inside", "outside", "inside", "outside"]
        );
        restored
            .sql(
                &format!("BEGIN; INSERT INTO parent VALUES(6,'{small}'); ROLLBACK"),
                &[],
            )
            .unwrap();
        assert_eq!(observations(&restored).len(), 5);
        drop(restored);
        assert_eq!(control.memory().used(), 0);
        if !matches!(provider, Provider::Memory) {
            let reopened = provider.open(&path.join("database"));
            assert_eq!(preserved::definitions(&reopened), expected);
            assert_eq!(
                observations(&reopened),
                ["inside", "inside", "outside", "inside", "outside"]
            );
        }
    }
}

#[rstest::rstest]
#[case::memory(Provider::Memory)]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn list_range_keys_preserve_equality_across_numeric_spellings(#[case] provider: Provider) {
    for (ty, left, right) in [
        ("numrange", "[1.0,3.00)", "[1.00,3.0)"),
        ("nummultirange", "{[1.0,3.00)}", "{[1.00,3.0)}"),
    ] {
        let directory = tempfile::tempdir().unwrap();
        let engine = provider.open(&directory.path().join("source"));
        engine.sql(&format!("CREATE TABLE parent(id integer, value {ty}) PARTITION BY LIST(value); CREATE TABLE inside PARTITION OF parent FOR VALUES IN('{left}','{right}'); CREATE TABLE outside PARTITION OF parent DEFAULT; INSERT INTO parent VALUES(1,'{left}'),(2,'{right}')"), &[]).unwrap();
        assert_eq!(observations(&engine), reference("list", ty));
        let catalog = engine
            .restored_catalog_read_view_with_control(
                &uqa_core::memory::ProductionControl::uncontrolled(),
            )
            .unwrap();
        let table = catalog
            .snapshot()
            .tables
            .get(&RelationIdentity::new("public", "inside"))
            .unwrap();
        let Some(PartitionBound::List(values)) = &table.hierarchy.partition_bound else {
            panic!("LIST bound")
        };
        assert_eq!(values.len(), 1);
        drop(catalog);
        let error = engine
            .sql(
                &format!("CREATE TABLE invalid PARTITION OF parent FOR VALUES IN('{right}')"),
                &[],
            )
            .unwrap_err();
        assert_eq!(error.sqlstate(), Some("42P17"));
    }
}
