//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Namespace identity is shared by public references, transaction undo and durable restoration.

use crate::tests::relation_lock_support::{reopen, sessions, sql};
use uqa_core::Value;

#[test]
fn direct_schema_api_and_public_recreation_allocate_new_namespace_identities() {
    let engine = crate::Engine::new();
    let public = engine.durable.schemas.read()["public"].tuple.unwrap();
    assert_eq!(public.oid, 2200);
    assert!(engine.drop_schema("public").unwrap());
    assert!(!engine.drop_schema("public").unwrap());
    assert!(engine.register_schema("public", false).unwrap());
    let recreated = engine.durable.schemas.read()["public"].tuple.unwrap();
    assert_ne!(recreated.oid, public.oid);
    assert_ne!(recreated.object_id, public.object_id);
    assert!(!engine.register_schema("public", true).unwrap());
    assert_eq!(
        engine.durable.schemas.read()["public"].tuple,
        Some(recreated)
    );
    assert_eq!(
        sql(&engine, "SELECT 'public'::regnamespace::oid AS id").rows[0]["id"],
        Value::Int(recreated.oid)
    );
}

#[test]
fn namespace_identity_survives_acl_owner_undo_and_reopen_but_not_recreation() {
    for provider in 0..3 {
        let (directory, first, second) = sessions(provider);
        sql(
            &first,
            "CREATE ROLE reader; CREATE ROLE owner; CREATE SCHEMA s",
        );
        let original = first.durable.schemas.read()["s"].tuple.unwrap();
        assert!((16_384..=i64::from(u32::MAX)).contains(&original.oid));
        let oid = |engine: &crate::Engine| {
            sql(engine, "SELECT 's'::regnamespace::oid AS id").rows[0]["id"].clone()
        };
        assert_eq!(oid(&first), Value::Int(original.oid));
        sql(
            &first,
            "GRANT USAGE ON SCHEMA s TO reader; ALTER SCHEMA s OWNER TO owner",
        );
        let updated = first.durable.schemas.read()["s"].tuple.unwrap();
        assert_eq!(updated.oid, original.oid);
        assert_eq!(updated.object_id, original.object_id);
        assert_ne!(updated.revision, original.revision);
        sql(
            &first,
            "BEGIN; SAVEPOINT namespace; DROP SCHEMA s; CREATE SCHEMA s",
        );
        let transient = first.durable.schemas.read()["s"].tuple.unwrap();
        assert_ne!(transient.oid, original.oid);
        assert_ne!(transient.object_id, original.object_id);
        assert_eq!(oid(&second), Value::Int(original.oid));
        sql(&first, "ROLLBACK TO namespace; COMMIT");
        assert_eq!(first.durable.schemas.read()["s"].tuple, Some(updated));
        sql(&first, "DROP SCHEMA s; CREATE SCHEMA s");
        let replacement = first.durable.schemas.read()["s"].tuple.unwrap();
        assert_ne!(replacement.oid, original.oid);
        assert_ne!(replacement.object_id, original.object_id);
        assert_eq!(oid(&second), Value::Int(replacement.oid));
        assert_eq!(
            sql(
                &second,
                &format!(
                    "SELECT has_schema_privilege('reader', {}::oid, 'USAGE') AS allowed",
                    original.oid
                )
            )
            .rows[0]["allowed"],
            Value::Null
        );
        drop(second);
        drop(first);
        let reopened = reopen(provider, &directory.path().join("table-locks.db"));
        assert_eq!(
            reopened.durable.schemas.read()["s"].tuple,
            Some(replacement)
        );
        assert_eq!(oid(&reopened), Value::Int(replacement.oid));
    }
}

#[test]
fn public_catalog_namespace_references_use_the_stored_oid() {
    for provider in 0..3 {
        let (_directory, first, _second) = sessions(provider);
        sql(&first, "CREATE SCHEMA s; CREATE TABLE s.items(id integer PRIMARY KEY); CREATE VIEW s.v AS SELECT id FROM s.items; CREATE MATERIALIZED VIEW s.mv AS SELECT id FROM s.items; CREATE SEQUENCE s.seq; CREATE DOMAIN s.label AS integer; CREATE FUNCTION s.answer() RETURNS integer LANGUAGE SQL AS $$ SELECT 42 $$");
        let oid = Value::Int(first.durable.schemas.read()["s"].tuple.unwrap().oid);
        let classes = sql(
            &first,
            "SELECT relnamespace AS id FROM pg_class WHERE relname IN ('items', 'v', 'mv', 'seq')",
        );
        assert_eq!(classes.rows.len(), 4);
        for row in classes.rows {
            assert_eq!(row["id"], oid);
        }
        for query in [
            "SELECT pronamespace AS id FROM pg_proc WHERE proname = 'answer'",
            "SELECT typnamespace AS id FROM pg_type WHERE typname = 'label'",
            "SELECT connamespace AS id FROM pg_constraint WHERE conname = 'items_pkey'",
        ] {
            let rows = sql(&first, query).rows;
            assert_eq!(rows.len(), 1, "{query}");
            assert_eq!(rows[0]["id"], oid, "{query}");
        }
        sql(&first, "SET search_path = s, public");
        let row = &sql(
            &first,
            "SELECT 'answer'::regproc::text AS proc, 's.answer()'::regprocedure::text AS signature",
        )
        .rows[0];
        assert_eq!(row["proc"], Value::Str("answer".into()));
        assert_eq!(row["signature"], Value::Str("answer()".into()));
    }
}
