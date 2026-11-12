//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[rstest::rstest]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn persistent_domains_keep_selected_constraints_and_later_sql_mutation_ownership(
    #[case] provider: Provider,
) {
    let directory = tempfile::tempdir().unwrap();
    let source = provider.open(&directory.path().join("domains"));
    source.sql("CREATE ROLE reader; CREATE DOMAIN positive AS integer DEFAULT 7 NOT NULL CHECK (VALUE > 0); GRANT USAGE ON DOMAIN positive TO reader; CREATE TABLE items(v positive); INSERT INTO items DEFAULT VALUES; GRANT SELECT ON items TO reader", &[]).unwrap();
    let expected = serde_json::to_value(&*source.durable.domains.read()).unwrap();
    let control = StorageReadControl::with_limit(32 << 20);
    let mut input = input(&source, &control);
    reach_types(&mut input, Kind::Domain);
    assert!(input.advance(16).unwrap().is_none());
    source
        .sql(
            "ALTER DOMAIN positive SET DEFAULT 9; CREATE DOMAIN later AS text",
            &[],
        )
        .unwrap();
    let session = finish(&mut input);
    assert_eq!(
        serde_json::to_value(&*session.engine().durable.domains.read()).unwrap(),
        expected
    );
    let restored = session.finish().unwrap();
    let occupied = control
        .memory()
        .reserve(control.memory().available())
        .unwrap();
    control.cancellation().cancel();
    restored.sql("SET ROLE reader", &[]).unwrap();
    assert_eq!(
        restored
            .sql("SELECT v::integer AS v FROM items", &[])
            .unwrap()
            .rows[0]["v"],
        Value::Int(7)
    );
    restored.sql("RESET ROLE; INSERT INTO items DEFAULT VALUES; ALTER DOMAIN positive ADD CONSTRAINT under_ten CHECK (VALUE < 10); CREATE DOMAIN another AS integer", &[]).unwrap();
    assert!(restored.durable.domains.read().contains_key("public.later"));
    assert!(restored
        .durable
        .domains
        .read()
        .contains_key("public.another"));
    assert_eq!(
        restored
            .sql("SELECT max(v::integer) AS v FROM items", &[])
            .unwrap()
            .rows[0]["v"],
        Value::Int(9)
    );
    assert_eq!(
        restored
            .sql("INSERT INTO items VALUES(11)", &[])
            .unwrap_err()
            .sqlstate(),
        Some("23514")
    );
    drop((restored, input, source));
    assert_eq!(control.memory().used(), occupied.bytes());
    drop(occupied);
    assert_eq!(control.memory().used(), 0);
}

#[rstest::rstest]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn persistent_domains_retire_private_roots_and_selected_transactions(#[case] provider: Provider) {
    let directory = tempfile::tempdir().unwrap();
    let source = provider.open(&directory.path().join("domains"));
    let mut sql = String::new();
    for i in 0..16 {
        write!(
            sql,
            "CREATE DOMAIN domain_{i} AS integer DEFAULT 7 NOT NULL CHECK (VALUE > 0);"
        )
        .unwrap();
    }
    source.sql(&sql, &[]).unwrap();
    let catalog = source.storage.catalog.as_ref().unwrap();
    let original = catalog.metadata_with_prefix("").unwrap();
    for terminal in ["cancel", "zero", "memory", "drop", "unwind"] {
        terminate(&source, Kind::Domain, terminal);
    }
    assert_eq!(catalog.metadata_with_prefix("").unwrap(), original);
    let marker = catalog.get_metadata("sql_domains_json").unwrap().unwrap();
    catalog
        .set_metadata("sql_domains_json", "{bad JSON")
        .unwrap();
    let control = StorageReadControl::with_limit(32 << 20);
    let mut input = input(&source, &control);
    let backend = Arc::clone(
        input
            .candidate
            .as_ref()
            .unwrap()
            .session
            .engine()
            .storage
            .backend
            .as_ref()
            .unwrap(),
    );
    let error = loop {
        match input.advance(64) {
            Err(error) => break error,
            Ok(None) => (),
            Ok(Some(_)) => panic!("corrupt domain marker published"),
        }
    };
    assert!(matches!(error, StorageBackendError::Serde(_)), "{error}");
    assert!(!backend.in_transaction());
    assert!(input.advance(1).is_err());
    assert_eq!(control.memory().used(), 0);
    catalog.set_metadata("sql_domains_json", &marker).unwrap();
    source
        .sql("CREATE DOMAIN recovered AS integer", &[])
        .unwrap();
}

#[rstest::rstest]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn persistent_domains_keep_delayed_cross_catalog_constraint_validation(#[case] provider: Provider) {
    let directory = tempfile::tempdir().unwrap();
    let source = provider.open(&directory.path().join("domains"));
    source.sql("CREATE TABLE items(v integer CONSTRAINT table_ck CHECK(v > 0)); CREATE DOMAIN positive AS integer CONSTRAINT domain_ck CHECK(VALUE > 0)", &[]).unwrap();
    let oid = source
        .sql(
            "SELECT oid FROM pg_constraint WHERE conname = 'table_ck'",
            &[],
        )
        .unwrap()
        .rows[0]["oid"]
        .clone();
    let Value::Int(oid) = oid else {
        panic!("constraint OID is integral");
    };
    let catalog = source.storage.catalog.as_ref().unwrap();
    let key = "uqa.sql.domain.v1:public.positive";
    let original = catalog.get_metadata(key).unwrap().unwrap();
    let mut corrupt: serde_json::Value = serde_json::from_str(&original).unwrap();
    corrupt["definition"]["checks"][0]["catalog_identity"]["oid"] = oid.into();
    catalog.set_metadata(key, &corrupt.to_string()).unwrap();
    let control = StorageReadControl::with_limit(32 << 20);
    let mut input = input(&source, &control);
    let backend = Arc::clone(
        input
            .candidate
            .as_ref()
            .unwrap()
            .session
            .engine()
            .storage
            .backend
            .as_ref()
            .unwrap(),
    );
    let mut domains_complete = false;
    let error = loop {
        domains_complete |= matches!(
            input.candidate.as_ref().unwrap().stage,
            Stage::Registries(_)
        );
        match input.advance(1) {
            Err(error) => break error,
            Ok(None) => (),
            Ok(Some(_)) => panic!("conflicting constraint identity published"),
        }
    };
    assert!(domains_complete);
    assert!(error.to_string().contains("conflicts"), "{error}");
    assert!(!backend.in_transaction());
    assert_eq!(control.memory().used(), 0);
    assert_eq!(
        catalog.get_metadata(key).unwrap().unwrap(),
        corrupt.to_string()
    );
    catalog.set_metadata(key, &original).unwrap();
    source
        .sql("ALTER DOMAIN positive SET DEFAULT 7", &[])
        .unwrap();
}
