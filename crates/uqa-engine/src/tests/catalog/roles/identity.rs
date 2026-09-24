//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Role OIDs identify stored objects across catalog references and transaction undo.

use crate::{
    tests::relation_lock_support::{sessions, sql},
    Engine,
};
use uqa_core::Value;

fn oid(engine: &Engine, name: &str) -> Value {
    sql(engine, &format!("SELECT to_regrole('{name}')::oid AS id")).rows[0]["id"].clone()
}

pub(super) use crate::tests::relation_lock_support::reopen;

#[test]
fn role_oids_follow_create_undo_recreate_and_reopen_for_every_provider() {
    for provider in 0..3 {
        let (directory, first, second) = sessions(provider);
        sql(&first, "CREATE ROLE named");
        let original = oid(&first, "named");
        let incarnation = first.durable.roles.read()["named"].object_id;
        assert_ne!(incarnation, [0; 16]);
        assert!(
            matches!(original, Value::Int(value) if (16_384..=i64::from(u32::MAX)).contains(&value))
        );
        sql(&first, "ALTER ROLE named LOGIN");
        assert_eq!(oid(&first, "named"), original);
        assert_eq!(first.durable.roles.read()["named"].object_id, incarnation);
        sql(
            &first,
            "BEGIN; SAVEPOINT old_role; DROP ROLE named; CREATE ROLE named",
        );
        assert_ne!(oid(&first, "named"), original);
        assert_ne!(first.durable.roles.read()["named"].object_id, incarnation);
        assert_eq!(oid(&second, "named"), original);
        sql(&first, "ROLLBACK TO old_role; COMMIT");
        assert_eq!(oid(&first, "named"), original);
        assert_eq!(first.durable.roles.read()["named"].object_id, incarnation);
        sql(&first, "DROP ROLE named; CREATE ROLE named");
        let replacement = oid(&first, "named");
        let replacement_incarnation = first.durable.roles.read()["named"].object_id;
        assert_ne!(replacement_incarnation, incarnation);
        assert_ne!(replacement, original);
        assert_eq!(oid(&second, "named"), replacement);
        drop(second);
        drop(first);
        let reopened = reopen(provider, &directory.path().join("table-locks.db"));
        assert_eq!(oid(&reopened, "named"), replacement);
        assert_eq!(
            reopened.durable.roles.read()["named"].object_id,
            replacement_incarnation
        );
    }
}

#[test]
fn role_catalog_references_use_persisted_owner_and_membership_oids() {
    let engine = Engine::new();
    sql(&engine, "CREATE ROLE owner; CREATE ROLE member; GRANT owner TO member; CREATE SCHEMA owned AUTHORIZATION owner; SET ROLE owner; CREATE TABLE owned.items(id integer); CREATE INDEX items_id ON owned.items(id); CREATE VIEW owned.visible AS SELECT id FROM owned.items; CREATE MATERIALIZED VIEW owned.saved AS SELECT id FROM owned.items; CREATE SEQUENCE owned.counter; CREATE FUNCTION owned.answer() RETURNS integer LANGUAGE sql AS 'SELECT 1'; RESET ROLE; CREATE SERVER remote FOREIGN DATA WRAPPER memory_fdw; CREATE FOREIGN TABLE owned.remote(id integer) SERVER remote; ALTER FOREIGN TABLE owned.remote OWNER TO owner");
    let owner = oid(&engine, "owner");
    let member = oid(&engine, "member");
    let rows = sql(&engine, "SELECT relname, relowner FROM pg_class WHERE relnamespace = (SELECT oid FROM pg_namespace WHERE nspname = 'owned')").rows;
    assert_eq!(rows.len(), 6);
    for row in rows {
        assert_eq!(row["relowner"], owner, "{:?}", row["relname"]);
    }
    assert_eq!(
        sql(
            &engine,
            "SELECT nspowner FROM pg_namespace WHERE nspname = 'owned'"
        )
        .rows[0]["nspowner"],
        owner
    );
    assert_eq!(
        sql(
            &engine,
            "SELECT proowner FROM pg_proc WHERE proname = 'answer'"
        )
        .rows[0]["proowner"],
        owner
    );
    let membership = &sql(
        &engine,
        "SELECT roleid, member, grantor FROM pg_auth_members",
    )
    .rows[0];
    assert_eq!(membership["roleid"], owner);
    assert_eq!(membership["member"], member);
    assert_eq!(membership["grantor"], Value::Int(10));
}
