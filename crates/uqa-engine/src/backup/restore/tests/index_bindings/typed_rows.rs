//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[rstest::rstest]
#[case::memory(Provider::Memory)]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn restored_index_bindings_preserve_boolean_domains_and_named_rows(#[case] provider: Provider) {
    let directory = tempfile::tempdir().unwrap();
    let repository = repository(&directory.path().join("repository"));
    let source = provider.open(&directory.path().join("source"));
    source.sql("CREATE DOMAIN truth AS boolean; CREATE DOMAIN nested_truth AS truth; CREATE TABLE flags(id integer, enabled nested_truth); CREATE TABLE items(id integer, enabled nested_truth); INSERT INTO flags VALUES(1,true),(-1,false); INSERT INTO items VALUES(1,true),(-1,false); CREATE FUNCTION row_key(x items) RETURNS integer LANGUAGE SQL IMMUTABLE AS $$ SELECT (x).id $$; CREATE FUNCTION row_enabled(x items) RETURNS nested_truth LANGUAGE SQL IMMUTABLE AS $$ SELECT (x).enabled $$; CREATE UNIQUE INDEX flag_saved ON flags(id) WHERE enabled; CREATE UNIQUE INDEX row_saved ON items(row_key(items)) WHERE row_enabled(items)", &[]).unwrap();
    assert_eq!(
        source
            .sql(
                "SELECT row_key(items) + 0 AS id FROM items WHERE row_enabled(items)",
                &[]
            )
            .unwrap()
            .rows[0]["id"],
        Value::Int(1)
    );
    assert_eq!(
        source
            .sql("INSERT INTO items VALUES(1,true)", &[])
            .unwrap_err()
            .sqlstate(),
        Some("23505")
    );
    source
        .sql(
            "BEGIN; ALTER TABLE items RENAME TO rolled_back_items; ROLLBACK",
            &[],
        )
        .unwrap();
    source.sql("ALTER DOMAIN truth RENAME TO renamed_truth; ALTER DOMAIN nested_truth RENAME TO renamed_nested_truth; ALTER FUNCTION row_key(items) RENAME TO renamed_key; ALTER FUNCTION row_enabled(items) RENAME TO renamed_enabled; ALTER TABLE flags RENAME TO renamed_flags; ALTER TABLE items RENAME TO renamed_items", &[]).unwrap();
    source.sql("CREATE SCHEMA moved", &[]).unwrap();
    let before_move = format!(
        "{:?}",
        source
            .durable
            .catalog_indexes
            .read()
            .values()
            .collect::<Vec<_>>()
    );
    source.sql("BEGIN; ALTER TABLE renamed_items SET SCHEMA moved; ALTER SCHEMA moved RENAME TO rolled_back_schema; ROLLBACK", &[]).unwrap();
    assert_eq!(
        format!(
            "{:?}",
            source
                .durable
                .catalog_indexes
                .read()
                .values()
                .collect::<Vec<_>>()
        ),
        before_move
    );
    source.sql("ALTER TABLE renamed_flags SET SCHEMA moved; ALTER TABLE renamed_items SET SCHEMA moved; ALTER SCHEMA moved RENAME TO retained", &[]).unwrap();
    let source = if matches!(provider, Provider::Memory) {
        source
    } else {
        drop(source);
        provider.open(&directory.path().join("source"))
    };
    assert_eq!(
        source
            .sql("INSERT INTO retained.renamed_items VALUES(1,true)", &[])
            .unwrap_err()
            .sqlstate(),
        Some("23505")
    );
    let indexes = source
        .durable
        .catalog_indexes
        .read()
        .values()
        .cloned()
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
    assert_eq!(
        format!(
            "{:?}",
            restored
                .durable
                .catalog_indexes
                .read()
                .values()
                .cloned()
                .collect::<Vec<_>>()
        ),
        format!("{indexes:?}"),
    );
    for table in ["retained.renamed_flags", "retained.renamed_items"] {
        assert_eq!(
            restored
                .sql(&format!("SELECT id FROM {table} ORDER BY id"), &[])
                .unwrap()
                .rows
                .iter()
                .map(|row| row["id"].clone())
                .collect::<Vec<_>>(),
            [Value::Int(-1), Value::Int(1)]
        );
        let error = restored
            .sql(&format!("INSERT INTO {table} VALUES(1,true)"), &[])
            .unwrap_err();
        assert_eq!(error.sqlstate(), Some("23505"));
        restored
            .sql(
                &format!("INSERT INTO {table} VALUES(1,false),(1,NULL),(2,true)"),
                &[],
            )
            .unwrap();
        let error = restored
            .sql(&format!("UPDATE {table} SET id=1 WHERE id=2"), &[])
            .unwrap_err();
        assert_eq!(error.sqlstate(), Some("23505"));
        restored
            .sql(
                &format!("BEGIN; INSERT INTO {table} VALUES(3,true); ROLLBACK"),
                &[],
            )
            .unwrap();
        assert!(restored
            .sql(&format!("SELECT id FROM {table} WHERE id=3"), &[])
            .unwrap()
            .rows
            .is_empty());
    }
    assert_eq!(restored.sql("SELECT renamed_key(renamed_items) + 0 AS id FROM retained.renamed_items WHERE renamed_enabled(renamed_items) ORDER BY id", &[]).unwrap().rows.iter().map(|row|row["id"].clone()).collect::<Vec<_>>(), [Value::Int(1),Value::Int(2)]);
    drop((restored, job));
    assert_eq!(control.memory().used(), 0);
}
