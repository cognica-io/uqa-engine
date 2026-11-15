//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

fn postgres_observations(engine: &Engine) -> String {
    let mut observations = Vec::new();
    for (sql, columns) in [
        ("SELECT 'enum-bound' AS kind, pg_get_expr(relpartbound,oid) AS value FROM pg_class WHERE oid='enum_low'::regclass", vec!["kind", "value"]),
        ("SELECT 'enum-row' AS kind,id,label::text AS label,tableoid::regclass::text AS target FROM enum_parent ORDER BY id", vec!["kind", "id", "label", "target"]),
        ("SELECT 'domain-bound' AS kind, pg_get_expr(relpartbound,oid) AS value FROM pg_class WHERE oid='domain_low'::regclass", vec!["kind", "value"]),
        ("SELECT 'domain-row' AS kind,id,tableoid::regclass::text AS target FROM domain_parent ORDER BY id", vec!["kind", "id", "target"]),
        ("SELECT 'function-volatility' AS kind,provolatile::text AS value FROM pg_proc WHERE oid='original_key(integer)'::regprocedure", vec!["kind", "value"]),
        ("SELECT 'function-row' AS kind,id,tableoid::regclass::text AS target FROM function_parent ORDER BY id", vec!["kind", "id", "target"]),
    ] {
        for row in engine.sql(sql, &[]).unwrap_or_else(|error| panic!("{sql}: {error:?}")).rows {
            observations.push(columns.iter().map(|column| match &row[*column] {
                Value::Str(text) => text.clone(),
                Value::Int(value) => value.to_string(),
                value => panic!("unexpected reference observation: {value:?}"),
            }).collect::<Vec<_>>().join("|"));
        }
    }
    observations.join("\n") + "\n"
}

fn definitions(engine: &Engine) -> serde_json::Value {
    let catalog = engine
        .restored_catalog_read_view_with_control(
            &uqa_core::memory::ProductionControl::uncontrolled(),
        )
        .unwrap();
    serde_json::to_value(catalog.snapshot().tables.iter().map(|(name, table)| {
        serde_json::json!({
            "name": name, "id": table.object_id, "oids": table.catalog_oids,
            "columns": &*table.columns, "hierarchy": &*table.hierarchy,
            "checks": &*table.checks, "keys": &*table.keys, "foreign_keys": &*table.foreign_keys,
        })
    }).collect::<Vec<_>>()).unwrap()
}

#[rstest::rstest]
#[case::memory(Provider::Memory)]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn restored_partition_bindings_preserve_original_constants_and_routing(#[case] provider: Provider) {
    let directory = tempfile::tempdir().unwrap();
    let repository = repository(&directory.path().join("repository"));
    let source = provider.open(&directory.path().join("source"));
    source.sql("
        CREATE TYPE temperature AS ENUM ('cold', 'warm', 'hot');
        CREATE TABLE enum_parent(id integer, label temperature) PARTITION BY RANGE(label);
        CREATE TABLE enum_low PARTITION OF enum_parent FOR VALUES FROM (MINVALUE) TO ('warm');
        CREATE TABLE enum_other PARTITION OF enum_parent DEFAULT;
        INSERT INTO enum_parent VALUES(1,'cold'),(2,'warm'),(3,'hot');
        ALTER TYPE temperature RENAME VALUE 'warm' TO 'mild';
        CREATE DOMAIN positive_key AS integer CHECK(VALUE>0);
        CREATE TABLE domain_parent(id positive_key) PARTITION BY RANGE(id);
        CREATE TABLE domain_low PARTITION OF domain_parent FOR VALUES FROM (1) TO (5);
        CREATE TABLE domain_other PARTITION OF domain_parent DEFAULT;
        INSERT INTO domain_parent VALUES(2);
        ALTER DOMAIN positive_key ADD CONSTRAINT later_minimum CHECK(VALUE>10) NOT VALID;
        INSERT INTO domain_parent VALUES(20);
        CREATE FUNCTION original_key(integer) RETURNS integer LANGUAGE sql IMMUTABLE AS 'SELECT $1';
        CREATE TABLE function_parent(id integer) PARTITION BY RANGE(original_key(id));
        CREATE TABLE function_low PARTITION OF function_parent FOR VALUES FROM (0) TO (10);
        CREATE TABLE function_other PARTITION OF function_parent DEFAULT;
        ALTER FUNCTION original_key(integer) VOLATILE;
        INSERT INTO function_parent VALUES(4),(12);
        CREATE TABLE list_parent(id bigint) PARTITION BY LIST(id);
        CREATE TABLE list_low PARTITION OF list_parent FOR VALUES IN(1,NULL);
        CREATE TABLE list_other PARTITION OF list_parent DEFAULT;
        INSERT INTO list_parent VALUES(1),(NULL),(2);
        CREATE TABLE hash_parent(id integer) PARTITION BY HASH(id);
        CREATE TABLE hash_even PARTITION OF hash_parent FOR VALUES WITH(MODULUS 2, REMAINDER 0);
        CREATE TABLE hash_odd PARTITION OF hash_parent FOR VALUES WITH(MODULUS 2, REMAINDER 1);
        INSERT INTO hash_parent VALUES(1),(2),(3),(4);
        CREATE TABLE range_parent(id integer, other integer) PARTITION BY RANGE(id,other);
        CREATE TABLE range_low PARTITION OF range_parent FOR VALUES FROM(0,MINVALUE) TO(10,MAXVALUE);
        INSERT INTO range_parent VALUES(3,4);
    ", &[]).unwrap();
    source
        .sql(
            "ALTER TABLE range_parent DETACH PARTITION range_low CONCURRENTLY",
            &[],
        )
        .unwrap();
    let oracle = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/parity/pg18/backup_partition_bindings_oracle.expected.txt"
    ));
    assert_eq!(postgres_observations(&source), oracle);
    let expected = definitions(&source);
    let queries = [
        "SELECT id,label,tableoid::regclass::text AS target FROM enum_parent ORDER BY id",
        "SELECT id,tableoid::regclass::text AS target FROM domain_parent ORDER BY id",
        "SELECT id,tableoid::regclass::text AS target FROM function_parent ORDER BY id",
        "SELECT id,tableoid::regclass::text AS target FROM list_parent ORDER BY id NULLS LAST",
        "SELECT id,tableoid::regclass::text AS target FROM hash_parent ORDER BY id",
        "SELECT id,other FROM range_low ORDER BY id",
    ];
    let rows = queries
        .iter()
        .map(|sql| source.sql(sql, &[]).unwrap().rows)
        .collect::<Vec<_>>();
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
    assert_eq!(definitions(&restored), expected);
    assert_eq!(postgres_observations(&restored), oracle);
    for (query, rows) in queries.iter().zip(&rows) {
        assert_eq!(
            &restored.sql(query, &[]).unwrap().rows,
            rows,
            "{provider:?}: {query}"
        );
    }
    drop(job);
    repository
        .remove_backup(root.backup(), &StorageReadControl::with_limit(32 << 20))
        .unwrap();
    restored.sql("INSERT INTO enum_parent VALUES(4,'mild'); INSERT INTO domain_parent VALUES(30); INSERT INTO function_parent VALUES(5); INSERT INTO list_parent VALUES(1); INSERT INTO hash_parent VALUES(5); INSERT INTO range_low VALUES(5,8)", &[]).unwrap();
    assert_eq!(
        restored
            .sql("SELECT count(*) AS n FROM function_low", &[])
            .unwrap()
            .rows[0]["n"],
        Value::Int(2)
    );
    assert_eq!(
        restored
            .sql("SELECT count(*) AS n FROM enum_other", &[])
            .unwrap()
            .rows[0]["n"],
        Value::Int(3)
    );
    for sql in [
        "INSERT INTO range_low VALUES(20,0)",
        "INSERT INTO domain_parent VALUES(3)",
    ] {
        assert_eq!(
            restored.sql(sql, &[]).unwrap_err().sqlstate(),
            Some("23514"),
            "{sql}"
        );
    }
    restored
        .sql("BEGIN; INSERT INTO list_parent VALUES(99); ROLLBACK", &[])
        .unwrap();
    assert!(restored
        .sql("SELECT id FROM list_other WHERE id=99", &[])
        .unwrap()
        .rows
        .is_empty());
    assert_eq!(definitions(&restored), expected);
    drop(restored);
    assert_eq!(control.memory().used(), 0);
    if !matches!(provider, Provider::Memory) {
        let reopened = provider.open(&path.join("database"));
        assert_eq!(definitions(&reopened), expected);
        assert_eq!(
            reopened
                .sql("SELECT count(*) AS n FROM function_low", &[])
                .unwrap()
                .rows[0]["n"],
            Value::Int(2)
        );
        assert_eq!(
            reopened
                .sql("INSERT INTO range_low VALUES(20,0)", &[])
                .unwrap_err()
                .sqlstate(),
            Some("23514")
        );
    }
}
