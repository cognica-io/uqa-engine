//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

fn definitions(engine: &Engine) -> serde_json::Value {
    let catalog = engine
        .restored_catalog_read_view_with_control(
            &uqa_core::memory::ProductionControl::uncontrolled(),
        )
        .unwrap();
    let snapshot = catalog.snapshot();
    serde_json::json!({
        "tables": snapshot.tables.iter().map(|(name, table)| serde_json::json!({
            "name": name, "object_id": table.object_id, "oids": table.catalog_oids,
            "columns": &*table.columns, "checks": &*table.checks,
        })).collect::<Vec<_>>(),
        "domains": snapshot.definitions.domains.iter().collect::<Vec<_>>(),
        "foreign": snapshot.definitions.foreign_tables.iter().map(|(name, table)| serde_json::json!({
            "name": name, "object_id": table.object_id, "oids": table.catalog_oids,
            "columns": &table.columns, "checks": &table.checks,
            "server": &table.server_name, "server_reference": &table.server_reference,
        })).collect::<Vec<_>>(),
    })
}

fn later_writes(engine: &Engine) {
    assert_eq!(
        engine
            .sql("SELECT id FROM retained.renamed_items ORDER BY id", &[])
            .unwrap()
            .rows
            .iter()
            .map(|row| row["id"].clone())
            .collect::<Vec<_>>(),
        [Value::Int(-1), Value::Int(1)]
    );
    for sql in [
        "INSERT INTO retained.renamed_items VALUES(-2,false)",
        "UPDATE retained.renamed_items SET flag=false WHERE id=1",
        "INSERT INTO old_domain_values VALUES(-2)",
        "SELECT (-1)::positive",
        "SELECT ROW(-1,true)::row_domain",
    ] {
        assert_eq!(
            engine.sql(sql, &[]).unwrap_err().sqlstate(),
            Some("23514"),
            "{sql}"
        );
    }
    assert_eq!(
        engine
            .sql("INSERT INTO latent VALUES(1)", &[])
            .unwrap_err()
            .sqlstate(),
        Some("22012")
    );
    engine.sql("BEGIN; INSERT INTO retained.renamed_items VALUES(2,true); INSERT INTO old_domain_values VALUES(2); ROLLBACK", &[]).unwrap();
    assert!(engine
        .sql("SELECT id FROM retained.renamed_items WHERE id=2", &[])
        .unwrap()
        .rows
        .is_empty());
    engine.sql("INSERT INTO retained.renamed_items VALUES(2,true),(3,NULL); INSERT INTO old_domain_values VALUES(2)", &[]).unwrap();
    assert_eq!(
        engine
            .sql("SELECT id FROM retained.renamed_items ORDER BY id", &[])
            .unwrap()
            .rows
            .len(),
        4
    );
}

#[rstest::rstest]
#[case::memory(Provider::Memory)]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn restored_constraint_bindings_preserve_named_rows_domains_and_not_valid_state(
    #[case] provider: Provider,
) {
    let directory = tempfile::tempdir().unwrap();
    let repository = repository(&directory.path().join("repository"));
    let source_path = directory.path().join("source");
    let source = provider.open(&source_path);
    source.sql("CREATE DOMAIN truth AS boolean; CREATE DOMAIN nested_truth AS truth CHECK(VALUE); CREATE DOMAIN positive AS integer CHECK(VALUE>0); CREATE DOMAIN later AS integer; CREATE TABLE old_domain_values(id later); INSERT INTO old_domain_values VALUES(-1); ALTER DOMAIN later ADD CONSTRAINT later_positive CHECK(VALUE>0) NOT VALID; CREATE TABLE items(id integer,flag truth); CREATE DOMAIN row_domain AS items CHECK((VALUE).id>0); INSERT INTO items VALUES(-1,false),(1,true); ALTER TABLE items ADD CONSTRAINT direct_guard CHECK(flag AND id>0) NOT VALID; CREATE FUNCTION row_guard(x items) RETURNS truth LANGUAGE SQL VOLATILE AS $$ SELECT (x).flag $$; ALTER TABLE items ADD CONSTRAINT row_guard_check CHECK(row_guard(items)) NOT VALID; CREATE TABLE latent(id integer CHECK(id/0>0)); CREATE SERVER remote FOREIGN DATA WRAPPER memory_fdw; CREATE FOREIGN TABLE remote_items(id integer CHECK(id>0),flag truth,CONSTRAINT flag_guard CHECK(flag)) SERVER remote", &[]).unwrap();
    source
        .sql(
            "ALTER TABLE items ADD CONSTRAINT collated_guard CHECK('true' COLLATE \"C\")",
            &[],
        )
        .unwrap();
    let initial = definitions(&source);
    source.sql("BEGIN; ALTER TABLE items RENAME TO rolled_back; ALTER DOMAIN truth RENAME TO rolled_back_truth; ROLLBACK", &[]).unwrap();
    assert_eq!(definitions(&source), initial);
    source.sql("ALTER DOMAIN truth RENAME TO renamed_truth; ALTER FUNCTION row_guard(items) RENAME TO renamed_guard; ALTER TABLE items RENAME TO renamed_items; CREATE SCHEMA moved; ALTER TABLE renamed_items SET SCHEMA moved; ALTER SCHEMA moved RENAME TO retained; ALTER FOREIGN TABLE remote_items RENAME TO renamed_remote", &[]).unwrap();
    let expected = definitions(&source);
    let source = if matches!(provider, Provider::Memory) {
        source
    } else {
        drop(source);
        let source = provider.open(&source_path);
        assert_eq!(definitions(&source), expected);
        source
    };
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
    drop(job);
    repository
        .remove_backup(root.backup(), &StorageReadControl::with_limit(32 << 20))
        .unwrap();
    later_writes(&restored);
    assert_eq!(definitions(&restored), expected);
    drop(restored);
    assert_eq!(control.memory().used(), 0);
    if !matches!(provider, Provider::Memory) {
        let reopened = provider.open(&path.join("database"));
        assert_eq!(definitions(&reopened), expected);
        assert_eq!(
            reopened
                .sql("SELECT id FROM retained.renamed_items ORDER BY id", &[])
                .unwrap()
                .rows
                .len(),
            4
        );
        assert_eq!(
            reopened
                .sql("INSERT INTO retained.renamed_items VALUES(-3,false)", &[])
                .unwrap_err()
                .sqlstate(),
            Some("23514")
        );
    }
}
