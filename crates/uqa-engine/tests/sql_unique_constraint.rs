//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Per-row UNIQUE constraint validation before `INSERT`.

use uqa_engine::Engine;

#[test]
fn unique_constraint_rejects_duplicate_value() {
    let eng = Engine::new();
    eng.sql(
        "CREATE TABLE accounts (id INTEGER PRIMARY KEY, email TEXT UNIQUE, name TEXT)",
        &[],
    )
    .unwrap();
    eng.sql(
        "INSERT INTO accounts (id, email, name) VALUES (1, 'a@x.com', 'alice')",
        &[],
    )
    .unwrap();
    let err = eng
        .sql(
            "INSERT INTO accounts (id, email, name) VALUES (2, 'a@x.com', 'alice2')",
            &[],
        )
        .unwrap_err();
    let msg = format!("{err:?}");
    assert!(
        msg.to_ascii_lowercase().contains("unique"),
        "expected UNIQUE error, got {msg}"
    );
}

#[test]
fn unique_constraint_allows_distinct_values() {
    let eng = Engine::new();
    eng.sql(
        "CREATE TABLE accounts (id INTEGER PRIMARY KEY, email TEXT UNIQUE, name TEXT)",
        &[],
    )
    .unwrap();
    eng.sql(
        "INSERT INTO accounts (id, email, name) VALUES (1, 'a@x.com', 'alice'), (2, 'b@x.com', 'bob')",
        &[],
    )
    .unwrap();
    let one = eng
        .get_document("accounts", 1)
        .unwrap()
        .expect("account row 1");
    let two = eng
        .get_document("accounts", 2)
        .unwrap()
        .expect("account row 2");
    assert_ne!(one.get("email"), two.get("email"));
}

/// The UNIQUE probe on insert resolves through the column value index
/// (built lazily by the first probe, maintained incrementally after):
/// duplicates of the first, a middle, and the last inserted value must
/// all still be rejected once hundreds of rows have flowed through the
/// incremental maintenance path, and a fresh value must still insert.
#[test]
fn unique_check_stays_correct_through_the_value_index() {
    let eng = Engine::new();
    eng.sql(
        "CREATE TABLE accounts (id INTEGER PRIMARY KEY, email TEXT UNIQUE)",
        &[],
    )
    .unwrap();
    for i in 0..500 {
        eng.sql(
            &format!("INSERT INTO accounts (id, email) VALUES ({i}, 'u{i}@x.com')"),
            &[],
        )
        .unwrap();
    }
    for dup in ["u0@x.com", "u250@x.com", "u499@x.com"] {
        let err = eng
            .sql(
                &format!("INSERT INTO accounts (id, email) VALUES (1000, '{dup}')"),
                &[],
            )
            .unwrap_err();
        let msg = format!("{err:?}").to_ascii_lowercase();
        assert!(msg.contains("unique"), "expected UNIQUE error, got {msg}");
    }
    eng.sql(
        "INSERT INTO accounts (id, email) VALUES (1000, 'fresh@x.com')",
        &[],
    )
    .unwrap();
}

/// Deletes and updates must free a unique value for reuse and claim the
/// new one, through the same incrementally maintained index the insert
/// probe reads.
#[test]
fn unique_value_frees_on_delete_and_moves_on_update() {
    let eng = Engine::new();
    eng.sql(
        "CREATE TABLE accounts (id INTEGER PRIMARY KEY, email TEXT UNIQUE)",
        &[],
    )
    .unwrap();
    eng.sql(
        "INSERT INTO accounts (id, email) VALUES (1, 'a@x.com'), (2, 'b@x.com')",
        &[],
    )
    .unwrap();
    eng.sql("DELETE FROM accounts WHERE id = 1", &[]).unwrap();
    eng.sql(
        "INSERT INTO accounts (id, email) VALUES (3, 'a@x.com')",
        &[],
    )
    .unwrap();
    eng.sql("UPDATE accounts SET email = 'c@x.com' WHERE id = 2", &[])
        .unwrap();
    eng.sql(
        "INSERT INTO accounts (id, email) VALUES (4, 'b@x.com')",
        &[],
    )
    .unwrap();
    let err = eng
        .sql(
            "INSERT INTO accounts (id, email) VALUES (5, 'c@x.com')",
            &[],
        )
        .unwrap_err();
    let msg = format!("{err:?}").to_ascii_lowercase();
    assert!(msg.contains("unique"), "expected UNIQUE error, got {msg}");
}

/// ON CONFLICT targeting a TEXT unique column resolves the existing row
/// through the indexed lookup and updates it in place instead of
/// duplicating -- the config-upsert shape downstream apps rely on.
#[test]
fn on_conflict_do_update_resolves_text_unique_target() {
    let eng = Engine::new();
    eng.sql(
        "CREATE TABLE kv (id INTEGER PRIMARY KEY, key TEXT UNIQUE, val TEXT)",
        &[],
    )
    .unwrap();
    eng.sql(
        "INSERT INTO kv (id, key, val) VALUES (1, 'config', 'v1')",
        &[],
    )
    .unwrap();
    eng.sql(
        "INSERT INTO kv (id, key, val) VALUES (2, 'config', 'v2') \
         ON CONFLICT (key) DO UPDATE SET val = EXCLUDED.val",
        &[],
    )
    .unwrap();
    let rows = eng.sql("SELECT key, val FROM kv", &[]).unwrap();
    assert_eq!(rows.rows.len(), 1, "upsert must not duplicate the row");
    assert_eq!(
        rows.rows[0].get("val").cloned(),
        Some(uqa_core::Value::Str("v2".into()))
    );
}

/// Temporal keys are outside the value index's semantics guard; the
/// probe must fall back to the evaluated scan and still reject the
/// duplicate.
#[test]
fn unique_temporal_column_still_rejects_duplicates_via_scan_fallback() {
    let eng = Engine::new();
    eng.sql(
        "CREATE TABLE events (id INTEGER PRIMARY KEY, at TIMESTAMP UNIQUE)",
        &[],
    )
    .unwrap();
    eng.sql(
        "INSERT INTO events (id, at) VALUES (1, '2026-01-01 00:00:00')",
        &[],
    )
    .unwrap();
    let err = eng
        .sql(
            "INSERT INTO events (id, at) VALUES (2, '2026-01-01 00:00:00')",
            &[],
        )
        .unwrap_err();
    let msg = format!("{err:?}").to_ascii_lowercase();
    assert!(msg.contains("unique"), "expected UNIQUE error, got {msg}");
}

/// Composite FOREIGN KEY validation narrows through the one indexed
/// reference column and must verify the remaining column on the
/// candidates: a child row whose first key half matches an existing
/// parent but whose second half does not must still be rejected.
#[test]
fn composite_foreign_key_verifies_non_pivot_columns() {
    let eng = Engine::new();
    eng.sql(
        "CREATE TABLE parents (id INTEGER PRIMARY KEY, code TEXT UNIQUE, region INTEGER, \
         UNIQUE (code, region))",
        &[],
    )
    .unwrap();
    eng.sql(
        "CREATE TABLE children (id INTEGER PRIMARY KEY, code TEXT, region INTEGER, \
         FOREIGN KEY (code, region) REFERENCES parents (code, region))",
        &[],
    )
    .unwrap();
    eng.sql(
        "INSERT INTO parents (id, code, region) VALUES (1, 'kr', 82)",
        &[],
    )
    .unwrap();
    eng.sql(
        "INSERT INTO children (id, code, region) VALUES (1, 'kr', 82)",
        &[],
    )
    .unwrap();
    let err = eng
        .sql(
            "INSERT INTO children (id, code, region) VALUES (2, 'kr', 81)",
            &[],
        )
        .unwrap_err();
    let msg = format!("{err:?}").to_ascii_lowercase();
    assert!(
        msg.contains("foreign key"),
        "expected FOREIGN KEY error, got {msg}"
    );
}

#[test]
fn composite_unique_is_tuple_scoped_and_update_safe() {
    let eng = Engine::new();
    eng.sql(
        "CREATE TABLE memberships (
            id INTEGER PRIMARY KEY,
            tenant TEXT,
            slug TEXT,
            CONSTRAINT memberships_tenant_slug_key UNIQUE (tenant, slug)
        )",
        &[],
    )
    .unwrap();
    eng.sql(
        "INSERT INTO memberships (id, tenant, slug) VALUES
            (1, 'a', 'one'), (2, 'a', 'two'), (3, 'b', 'one'),
            (4, 'a', NULL), (5, 'a', NULL)",
        &[],
    )
    .unwrap();

    let duplicate = eng
        .sql(
            "INSERT INTO memberships (id, tenant, slug) VALUES (6, 'a', 'one')",
            &[],
        )
        .unwrap_err();
    let message = duplicate.to_string().to_ascii_lowercase();
    assert!(message.contains("memberships_tenant_slug_key"), "{message}");

    let update = eng
        .sql("UPDATE memberships SET slug = 'one' WHERE id = 2", &[])
        .unwrap_err();
    assert!(update.to_string().to_ascii_lowercase().contains("unique"));
    let row = eng
        .sql("SELECT slug FROM memberships WHERE id = 2", &[])
        .unwrap();
    assert_eq!(row.rows[0]["slug"], uqa_core::Value::Str("two".into()));
}

#[test]
fn composite_primary_key_is_unique_and_every_member_is_not_null() {
    let eng = Engine::new();
    eng.sql(
        "CREATE TABLE ledger (
            tenant TEXT,
            entry INTEGER,
            value TEXT,
            CONSTRAINT ledger_pkey PRIMARY KEY (tenant, entry)
        )",
        &[],
    )
    .unwrap();
    eng.sql(
        "INSERT INTO ledger (tenant, entry, value) VALUES
            ('a', 1, 'first'), ('a', 2, 'second'), ('b', 1, 'third')",
        &[],
    )
    .unwrap();

    let duplicate = eng
        .sql(
            "INSERT INTO ledger (tenant, entry, value) VALUES ('a', 1, 'duplicate')",
            &[],
        )
        .unwrap_err();
    assert!(duplicate
        .to_string()
        .to_ascii_lowercase()
        .contains("primary key"));

    for sql in [
        "INSERT INTO ledger (tenant, entry, value) VALUES (NULL, 3, 'bad')",
        "INSERT INTO ledger (tenant, entry, value) VALUES ('a', NULL, 'bad')",
    ] {
        let error = eng.sql(sql, &[]).unwrap_err();
        assert_eq!(error.sqlstate(), Some("23502"), "{sql}: {error}");
    }
}

#[test]
fn unique_nulls_not_distinct_rejects_repeated_null_tuple() {
    let eng = Engine::new();
    eng.sql(
        "CREATE TABLE contacts (
            id INTEGER PRIMARY KEY,
            tenant TEXT,
            email TEXT,
            UNIQUE NULLS NOT DISTINCT (tenant, email)
        )",
        &[],
    )
    .unwrap();
    eng.sql(
        "INSERT INTO contacts (id, tenant, email) VALUES (1, 'a', NULL)",
        &[],
    )
    .unwrap();
    let error = eng
        .sql(
            "INSERT INTO contacts (id, tenant, email) VALUES (2, 'a', NULL)",
            &[],
        )
        .unwrap_err();
    assert!(error.to_string().to_ascii_lowercase().contains("unique"));
}

#[test]
fn composite_keys_survive_sqlite_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("composite-keys.db");
    {
        let eng = Engine::open(&path).unwrap();
        eng.sql(
            "CREATE TABLE durable (
                id INTEGER PRIMARY KEY,
                tenant TEXT,
                slug TEXT,
                CONSTRAINT durable_tenant_slug_key UNIQUE (tenant, slug)
            )",
            &[],
        )
        .unwrap();
        eng.sql(
            "INSERT INTO durable (id, tenant, slug) VALUES (1, 'a', 'one')",
            &[],
        )
        .unwrap();
    }

    let eng = Engine::open(&path).unwrap();
    let error = eng
        .sql(
            "INSERT INTO durable (id, tenant, slug) VALUES (2, 'a', 'one')",
            &[],
        )
        .unwrap_err();
    let message = error.to_string().to_ascii_lowercase();
    assert!(message.contains("durable_tenant_slug_key"), "{message}");
}

#[test]
fn integer_primary_key_defaults_choose_the_physical_document_id_for_every_insert_source() {
    let eng = Engine::new();
    eng.sql(
        "CREATE TABLE values_target (
            id INTEGER PRIMARY KEY DEFAULT 7,
            payload TEXT
        )",
        &[],
    )
    .unwrap();
    eng.sql(
        "INSERT INTO values_target (payload) VALUES ('from-values')",
        &[],
    )
    .unwrap();
    assert_eq!(
        eng.get_document("values_target", 7)
            .unwrap()
            .expect("DEFAULT primary key must address the inserted document")["payload"],
        uqa_core::Value::Str("from-values".into())
    );

    eng.sql(
        "CREATE TABLE select_target (
            id INTEGER PRIMARY KEY DEFAULT 9,
            payload TEXT
        )",
        &[],
    )
    .unwrap();
    eng.sql("CREATE TABLE source_rows (payload TEXT)", &[])
        .unwrap();
    eng.sql(
        "INSERT INTO source_rows (payload) VALUES ('from-select')",
        &[],
    )
    .unwrap();
    eng.sql(
        "INSERT INTO select_target (payload)
         SELECT payload FROM source_rows",
        &[],
    )
    .unwrap();
    assert_eq!(
        eng.get_document("select_target", 9)
            .unwrap()
            .expect("SELECT-source DEFAULT primary key must address the inserted document")
            ["payload"],
        uqa_core::Value::Str("from-select".into())
    );
}

#[test]
fn typed_dml_rejects_unknown_and_duplicate_target_columns() {
    let eng = Engine::new();
    eng.sql(
        "CREATE TABLE typed_rows (id INTEGER PRIMARY KEY, payload TEXT)",
        &[],
    )
    .unwrap();
    eng.sql(
        "INSERT INTO typed_rows (id, payload) VALUES (1, 'before')",
        &[],
    )
    .unwrap();

    for sql in [
        "INSERT INTO typed_rows (id, misspelled) VALUES (2, 'bad')",
        "UPDATE typed_rows SET misspelled = 'bad' WHERE id = 1",
        "INSERT INTO typed_rows (id, payload) VALUES (1, 'bad')
         ON CONFLICT (id) DO UPDATE SET misspelled = EXCLUDED.payload",
    ] {
        let error = eng.sql(sql, &[]).unwrap_err();
        assert!(
            error
                .to_string()
                .to_ascii_lowercase()
                .contains("unknown column"),
            "unexpected error for {sql}: {error}"
        );
    }

    for sql in [
        "INSERT INTO typed_rows (id, id) VALUES (2, 3)",
        "UPDATE typed_rows SET payload = 'one', payload = 'two' WHERE id = 1",
    ] {
        let error = eng.sql(sql, &[]).unwrap_err();
        assert!(
            error
                .to_string()
                .to_ascii_lowercase()
                .contains("more than once"),
            "unexpected error for {sql}: {error}"
        );
    }

    let row = eng
        .sql("SELECT payload FROM typed_rows WHERE id = 1", &[])
        .unwrap();
    assert_eq!(
        row.rows[0]["payload"],
        uqa_core::Value::Str("before".into())
    );
    assert!(eng.get_document("typed_rows", 2).unwrap().is_none());
}

#[test]
fn insert_select_without_a_target_list_maps_values_by_target_position() {
    let eng = Engine::new();
    eng.sql(
        "CREATE TABLE positional_target (id INTEGER PRIMARY KEY, payload TEXT)",
        &[],
    )
    .unwrap();
    eng.sql(
        "CREATE TABLE positional_source (source_id INTEGER, source_payload TEXT)",
        &[],
    )
    .unwrap();
    eng.sql(
        "INSERT INTO positional_source (source_id, source_payload)
         VALUES (41, 'mapped')",
        &[],
    )
    .unwrap();

    eng.sql(
        "INSERT INTO positional_target
         SELECT source_id, source_payload FROM positional_source",
        &[],
    )
    .unwrap();

    let document = eng
        .get_document("positional_target", 41)
        .unwrap()
        .expect("source values must map to target columns by position");
    assert_eq!(document["id"], uqa_core::Value::Int(41));
    assert_eq!(document["payload"], uqa_core::Value::Str("mapped".into()));
    assert!(!document.contains_key("source_id"));
    assert!(!document.contains_key("source_payload"));
}

#[path = "sql_unique_constraint/indexes.rs"]
mod indexes;
