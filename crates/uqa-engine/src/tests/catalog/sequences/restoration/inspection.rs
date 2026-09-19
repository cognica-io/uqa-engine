//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Public sequence inspection must not install definitions without their authorization view.

use crate::tests::relation_lock_support::{sessions, sql};
use uqa_core::Value;

#[test]
fn public_sequence_inspection_keeps_live_catalog_authority_coherent() {
    for provider in 0..3 {
        for isolation in ["READ COMMITTED", "REPEATABLE READ", "SERIALIZABLE"] {
            for api in ["snapshot", "names", "state"] {
                let (_directory, engine, peer) = sessions(provider);
                sql(&engine, "CREATE SEQUENCE ids");
                sql(&engine, &format!("BEGIN ISOLATION LEVEL {isolation}; SELECT * FROM t; CREATE ROLE private_owner; CREATE SEQUENCE private_ids; ALTER SEQUENCE private_ids OWNER TO private_owner; CREATE TEMP SEQUENCE temp_ids"));
                sql(&peer, "CREATE ROLE new_owner; ALTER SEQUENCE ids OWNER TO new_owner; CREATE SEQUENCE peer_ids; SELECT setval('ids', 77); INSERT INTO t VALUES (2)");
                match api {
                    "snapshot" => {
                        let values = engine.try_sequences_snapshot().unwrap();
                        assert_eq!(values["public.ids"].current, 77);
                        assert!(values.contains_key("public.private_ids"));
                        assert!(values.contains_key("public.peer_ids"));
                        assert!(values.keys().any(|name| name.ends_with(".temp_ids")));
                    }
                    "names" => {
                        let values = engine.list_sequences().unwrap();
                        assert!(values.contains(&"public.ids".into()));
                        assert!(values.contains(&"public.private_ids".into()));
                        assert!(values.contains(&"public.peer_ids".into()));
                        assert!(values.iter().any(|name| name.ends_with(".temp_ids")));
                    }
                    "state" => {
                        assert_eq!(engine.sequence_state("ids").unwrap().unwrap().1.current, 77);
                        assert!(engine.sequence_state("private_ids").unwrap().is_some());
                        assert!(engine.sequence_state("peer_ids").unwrap().is_some());
                        assert!(engine.sequence_state("pg_temp.temp_ids").unwrap().is_some());
                    }
                    _ => unreachable!(),
                }
                let roles = engine.durable.roles.read();
                for security in engine.durable.sequence_security.read().values() {
                    let security = security.resolve(&roles).unwrap();
                    assert!(roles.contains_key(&security.role_owner), "{provider}: {isolation}: {api}: sequence owner {} is absent from the live role view", security.role_owner);
                }
                drop(roles);
                assert_eq!(
                    sql(&engine, "SELECT count(*) AS n FROM t").rows[0]["n"],
                    Value::Int(if isolation == "READ COMMITTED" { 2 } else { 1 })
                );
                sql(&engine, "ROLLBACK");
            }
        }
    }
}

#[test]
fn sequence_enumeration_preserves_a_retained_query_catalog() {
    let mut engine = crate::Engine::new();
    sql(&engine, "CREATE SEQUENCE retained_ids");
    let retained = std::sync::Arc::new(engine.durable.snapshot());
    sql(&engine, "CREATE SEQUENCE later_ids");
    engine.query_catalog_snapshot = Some(retained);
    assert_eq!(engine.list_sequences().unwrap(), ["public.retained_ids"]);
}
