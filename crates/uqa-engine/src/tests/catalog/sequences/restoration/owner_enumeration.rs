//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use crate::tests::relation_lock_support::{sessions, sql};
use std::collections::BTreeSet;
use uqa_core::{RelationIdentity, Value};

#[test]
fn live_sequence_refresh_keeps_roles_and_private_dependencies_with_current_values() {
    for provider in 0..3 {
        for isolation in ["READ COMMITTED", "REPEATABLE READ", "SERIALIZABLE"] {
            for api in ["refresh", "table", "column"] {
                let (_directory, engine, peer) = sessions(provider);
                sql(&engine, "CREATE SEQUENCE ids OWNED BY t.v; CREATE TEMP TABLE temp_t(v integer); CREATE TEMP SEQUENCE temp_ids OWNED BY temp_t.v");
                let table = engine.try_table("t").unwrap().unwrap();
                let column_id = table.columns.read()[0].object_id.unwrap();
                let temporary = engine.try_table("temp_t").unwrap().unwrap();
                let temporary_column = temporary.columns.read()[0].object_id.unwrap();
                sql(&engine, &format!("BEGIN ISOLATION LEVEL {isolation}; SELECT * FROM t; SAVEPOINT private_definition; CREATE ROLE private_reader; CREATE SEQUENCE private_ids OWNED BY t.v; GRANT SELECT ON SEQUENCE private_ids TO private_reader"));
                sql(&peer, "CREATE ROLE new_owner; CREATE SEQUENCE peer_ids; ALTER SEQUENCE peer_ids OWNER TO new_owner; SELECT setval('ids', 77); INSERT INTO t VALUES (2)");
                let owned = |table, attribute| {
                    if api == "column" {
                        engine
                            .sequence_names_owned_by_column(table, attribute)
                            .unwrap()
                    } else {
                        engine
                            .sequence_names_owned_by_tables(&BTreeSet::from([table]))
                            .unwrap()
                    }
                };
                if api == "refresh" {
                    engine.refresh_sequences_from_catalog().unwrap();
                }
                assert_eq!(
                    owned(table.object_id(), column_id),
                    BTreeSet::from(["public.ids".into(), "public.private_ids".into()])
                );
                let temporary_names = owned(temporary.object_id(), temporary_column);
                assert_eq!(temporary_names.len(), 1);
                assert!(temporary_names.first().unwrap().ends_with(".temp_ids"));
                let roles = engine.durable.roles.read();
                assert!(roles.contains_key("private_reader"));
                for security in engine.durable.sequence_security.read().values() {
                    let security = security.resolve(&roles).unwrap();
                    assert!(
                        roles.contains_key(&security.role_owner),
                        "{provider}: {isolation}: {api}: missing live owner {}",
                        security.role_owner
                    );
                    for entry in security.acl.iter().flatten() {
                        if let Some(role) = entry.role.role_name() {
                            assert!(
                                roles.contains_key(role),
                                "{provider}: {isolation}: {api}: missing live grantee {role}"
                            );
                        }
                    }
                }
                drop(roles);
                assert_eq!(
                    engine.durable.sequences.read()[&RelationIdentity::new("public", "ids")]
                        .current,
                    77
                );
                sql(&engine, "ROLLBACK TO private_definition");
                assert_eq!(
                    owned(table.object_id(), column_id),
                    BTreeSet::from(["public.ids".into()])
                );
                assert_eq!(
                    sql(&engine, "SELECT count(*) AS n FROM t").rows[0]["n"],
                    Value::Int(if isolation == "READ COMMITTED" { 2 } else { 1 })
                );
                sql(&engine, "ROLLBACK");
            }
        }
    }
}
