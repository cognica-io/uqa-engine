//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Current definition refresh keeps private sequence records alongside committed roles.

use super::{snapshot, RelationIdentity};
use crate::tests::relation_lock_support::{sessions, sql};
use std::sync::Arc;
use uqa_core::Value;

#[test]
fn fixed_catalog_refresh_preserves_private_sequence_records_and_temporary_entries() {
    for provider in 0..3 {
        for isolation in ["REPEATABLE READ", "SERIALIZABLE"] {
            for finish in ["COMMIT", "ROLLBACK", "ROLLBACK TO before_private; COMMIT"] {
                let (_directory, engine, peer) = sessions(provider);
                sql(&engine, "CREATE ROLE reader; CREATE SEQUENCE altered_ids; CREATE SEQUENCE dropped_ids; CREATE SEQUENCE old_ids; CREATE SEQUENCE shared_ids");
                sql(&engine, &format!("BEGIN ISOLATION LEVEL {isolation}; SELECT * FROM t; SAVEPOINT before_private; CREATE ROLE private_owner; CREATE SEQUENCE private_ids; ALTER SEQUENCE private_ids OWNER TO private_owner; CREATE TEMP SEQUENCE temp_ids; ALTER SEQUENCE altered_ids INCREMENT 7 CACHE 4; GRANT USAGE ON SEQUENCE altered_ids TO reader; DROP SEQUENCE dropped_ids; ALTER SEQUENCE old_ids RENAME TO renamed_ids"));
                let mut expected = snapshot(&engine);
                sql(&peer, "CREATE ROLE peer_role; CREATE SEQUENCE peer_ids; ALTER SEQUENCE shared_ids INCREMENT 11");
                let committed = snapshot(&peer);
                for name in ["peer_ids", "shared_ids"] {
                    let relation = RelationIdentity::new("public", name);
                    expected
                        .sequences
                        .insert(relation.clone(), committed.sequences[&relation]);
                    expected
                        .object_ids
                        .insert(relation.clone(), committed.object_ids[&relation]);
                    expected
                        .persistence
                        .insert(relation.clone(), committed.persistence[&relation]);
                    expected
                        .security
                        .insert(relation.clone(), committed.security[&relation].clone());
                }
                engine.synchronize_table_catalog().unwrap();
                assert_eq!(
                    snapshot(&engine),
                    expected,
                    "{provider}: {isolation}: {finish}"
                );
                assert!(engine.durable.roles.read().contains_key("private_owner"));
                assert!(engine.durable.roles.read().contains_key("peer_role"));
                sql(&engine, finish);
                let reopened = engine.new_session().unwrap();
                let actual = reopened.try_sequences_snapshot().unwrap();
                let retained_private = finish == "COMMIT";
                assert_eq!(actual.contains_key("public.private_ids"), retained_private);
                assert_eq!(actual.contains_key("public.renamed_ids"), retained_private);
                assert_eq!(actual.contains_key("public.old_ids"), !retained_private);
                assert_eq!(actual.contains_key("public.dropped_ids"), !retained_private);
                assert_eq!(
                    actual["public.altered_ids"].increment,
                    if retained_private { 7 } else { 1 }
                );
                assert_eq!(actual["public.shared_ids"].increment, 11);
                assert!(actual.contains_key("public.peer_ids"));
                assert!(!actual.keys().any(|name| name.ends_with(".temp_ids")));
            }
        }
    }
}

#[test]
fn callback_catalog_refresh_can_resolve_a_private_sequence() {
    for provider in 0..3 {
        for isolation in ["REPEATABLE READ", "SERIALIZABLE"] {
            let (_directory, engine, peer) = sessions(provider);
            let engine = Arc::new(engine);
            let peer = Arc::new(peer);
            let callback_engine = Arc::downgrade(&engine);
            engine
                .register_scalar_function("publish_then_allocate", move |_: &[Value]| {
                    peer.sql("CREATE ROLE peer_role", &[])?;
                    let engine = callback_engine.upgrade().unwrap();
                    let result = engine.sql("SELECT nextval('private_ids') AS n", &[])?;
                    Ok(result.rows[0]["n"].clone())
                })
                .unwrap();
            sql(&engine, &format!("BEGIN ISOLATION LEVEL {isolation}; SELECT * FROM t; CREATE SEQUENCE private_ids"));
            assert_eq!(
                sql(&engine, "SELECT publish_then_allocate() AS n").rows[0]["n"],
                Value::Int(1)
            );
            sql(&engine, "ROLLBACK");
            assert!(!engine
                .new_session()
                .unwrap()
                .try_sequences_snapshot()
                .unwrap()
                .contains_key("public.private_ids"));
        }
    }
}
