//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Server deletion retains identities and dependency state through waits, rollback and reopen.

use super::servers::open;
use crate::tests::relation_lock_support::{after_shared_wait_with_release, sessions, sql};
use crate::Engine;
use serde_json::{json, Value};
use uqa_execution::row_locks::{shared_objects::SharedCatalogLock, RelationLockMode};

#[test]
fn foreign_server_deletion_restores_metadata_and_dependencies_on_undo() {
    for provider in 0..4 {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("server-deletion.db");
        let engine = open(provider, &path);
        sql(&engine, "CREATE ROLE server_owner; SET ROLE server_owner; CREATE SERVER source TYPE 'original' VERSION '' FOREIGN DATA WRAPPER memory_fdw OPTIONS (source 'original'); RESET ROLE; CREATE FOREIGN TABLE remote (v integer) SERVER source OPTIONS (source 'memory'); CREATE VIEW dependent AS SELECT v FROM remote");
        let original = engine.durable.foreign_servers.read()["source"].clone();
        let retained = engine.catalog_read_view();
        let peer = (provider > 0).then(|| engine.new_session().unwrap());
        sql(&engine, "BEGIN; SAVEPOINT before_drop; DROP SERVER source CASCADE; CREATE SERVER source TYPE 'replacement' FOREIGN DATA WRAPPER memory_fdw");
        let replacement = engine.durable.foreign_servers.read()["source"].clone();
        assert_ne!(replacement.metadata.oid, original.metadata.oid);
        assert_ne!(replacement.metadata.object_id, original.metadata.object_id);
        assert_eq!(
            retained.snapshot().definitions.foreign_servers["source"],
            original
        );
        assert_eq!(retained.snapshot().definitions.foreign_tables.len(), 1);
        assert!(engine.list_foreign_tables().unwrap().is_empty());
        if let Some(peer) = &peer {
            assert_eq!(
                peer.foreign_server("source").unwrap().unwrap().options,
                original.options
            );
        }
        sql(&engine, "ROLLBACK TO before_drop; COMMIT");
        assert_eq!(engine.durable.foreign_servers.read()["source"], original);
        assert!(engine.foreign_table("remote").unwrap().is_some());
        assert_eq!(
            engine
                .sql("DROP SERVER source", &[])
                .unwrap_err()
                .sqlstate(),
            Some("2BP01")
        );
        sql(&engine, "DROP SERVER source CASCADE; DROP ROLE server_owner; CREATE SERVER source TYPE 'committed' VERSION NULL FOREIGN DATA WRAPPER memory_fdw OPTIONS (source 'committed')");
        let committed = engine.durable.foreign_servers.read()["source"].clone();
        assert_ne!(committed.metadata.oid, original.metadata.oid);
        assert_ne!(committed.metadata.object_id, original.metadata.object_id);
        if let Some(peer) = &peer {
            assert_eq!(
                peer.foreign_server("source").unwrap().unwrap().options,
                committed.options
            );
            assert!(peer.foreign_table("remote").unwrap().is_none());
        }
        drop(retained);
        drop(peer);
        drop(engine);
        if provider > 0 {
            let reopened = open(provider, &path);
            assert_eq!(reopened.durable.foreign_servers.read()["source"], committed);
            assert!(reopened.foreign_table("remote").unwrap().is_none());
            assert!(reopened.sql("SELECT * FROM dependent", &[]).is_err());
        }
    }
}

#[test]
fn direct_foreign_server_deletion_obeys_authority_restrict_and_read_only_state() {
    for provider in 0..4 {
        let directory = tempfile::tempdir().unwrap();
        let engine = open(provider, &directory.path().join("direct-drop.db"));
        assert!(!engine.drop_foreign_server("absent").unwrap());
        assert!(engine.take_sql_notices().is_empty());
        sql(&engine, "CREATE ROLE server_owner; CREATE ROLE other; SET ROLE server_owner; CREATE SERVER source FOREIGN DATA WRAPPER memory_fdw; RESET ROLE; CREATE FOREIGN TABLE remote(v integer) SERVER source OPTIONS(source 'memory')");
        sql(&engine, "SET ROLE other");
        assert_eq!(
            engine.drop_foreign_server("source").unwrap_err(),
            "must be owner of foreign server source"
        );
        sql(&engine, "RESET ROLE");
        assert!(engine
            .drop_foreign_server("source")
            .unwrap_err()
            .contains("other objects depend on it"));
        assert!(engine.foreign_table("remote").unwrap().is_some());
        sql(&engine, "BEGIN READ ONLY");
        assert!(engine
            .drop_foreign_server("source")
            .unwrap_err()
            .contains("read-only transaction"));
        sql(
            &engine,
            "ROLLBACK; DROP FOREIGN TABLE remote; SET ROLE server_owner",
        );
        assert!(engine.drop_foreign_server("source").unwrap());
        assert!(!engine.drop_foreign_server("source").unwrap());
        sql(&engine, "RESET ROLE; DROP ROLE server_owner");
    }
}

#[test]
fn foreign_server_deletion_matches_observed_postgresql_object_waits() {
    let oracle: Value = serde_json::from_str(include_str!(
        "../../../../../../tests/parity/pg18/drop_foreign_server_concurrency_oracle.expected.json"
    ))
    .unwrap();
    for provider in 0..3 {
        for schedule in oracle["schedules"].as_array().unwrap() {
            run_object_wait(provider, schedule);
        }
    }
}

fn run_object_wait(provider: usize, schedule: &Value) {
    let (_directory, first, second) = sessions(provider);
    sql(
        &first,
        "CREATE ROLE ds414_wait_owner; CREATE ROLE ds414_wait_other",
    );
    execute_messages(&first, &schedule["before"]);
    let originals = first.durable.foreign_servers.snapshot();
    let inspector = first.new_session().unwrap();
    execute_messages(&first, &schedule["session_a"]);
    execute_messages(&second, &schedule["session_b_before"]);
    let target = schedule["wait_for"].as_str().unwrap();
    let wait = SharedCatalogLock::Object {
        class_id: 1417,
        oid: originals[target].metadata.oid,
    };
    let (second, result) = after_shared_wait_with_release(
        &first,
        second,
        schedule["session_b"].as_str().unwrap(),
        wait,
        || {
            for (name, definition) in originals.iter().filter(|(name, _)| name.as_str() != target) {
                let key = first
                    .row_locks
                    .shared_catalog_key(SharedCatalogLock::Object {
                        class_id: 1417,
                        oid: definition.metadata.oid,
                    });
                let probe = inspector.row_locks.try_acquire_scoped_relation(
                    inspector.session_id,
                    key,
                    RelationLockMode::AccessExclusive,
                    (0, 1),
                    &inspector.runtime.cancellation,
                )?;
                let held = schedule["held_before_wait"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|held| held.as_str() == Some(name));
                assert_eq!(
                    probe.is_none(),
                    held,
                    "{}: written-order lock on {name}",
                    schedule["id"]
                );
            }
            first.sql(schedule["finish_a"].as_str().unwrap(), &[])
        },
    );
    if schedule["error"].is_null() {
        assert_eq!(
            result.unwrap().command_tag.as_deref(),
            schedule["command_tag"].as_str(),
            "{}",
            schedule["id"]
        );
    } else {
        let error = result.unwrap_err();
        assert_eq!(
            json!({"severity":"ERROR","sqlstate":error.sqlstate(),"message":error.to_string(),"detail":error.detail(),"hint":error.hint()}),
            schedule["error"],
            "{}",
            schedule["id"]
        );
    }
    let notices = second.take_sql_notices().into_iter().map(|notice| json!({"severity":notice.level.as_str(), "sqlstate":notice.sqlstate,"message":notice.message,"detail":notice.detail,"hint":notice.hint})).collect::<Vec<_>>();
    assert_eq!(json!(notices), schedule["notices"], "{}", schedule["id"]);
    sql(&second, "RESET ROLE");
    let roles = second.durable.roles.read();
    let servers = second
        .durable
        .foreign_servers
        .read()
        .values()
        .map(|server| {
            let owner = if server.metadata.owner.oid == 10 {
                "$bootstrap"
            } else {
                roles
                    .iter()
                    .find_map(|(name, role)| {
                        (role.oid == server.metadata.owner.oid
                            && role.object_id == server.metadata.owner.object_id)
                            .then_some(name.as_str())
                    })
                    .unwrap()
            };
            json!([server.name, server.metadata.server_type, owner])
        })
        .collect::<Vec<_>>();
    assert_eq!(
        json!(servers),
        schedule["final_servers"],
        "{}",
        schedule["id"]
    );
}

fn execute_messages(engine: &Engine, messages: &Value) {
    for statement in messages.as_array().unwrap() {
        sql(engine, statement.as_str().unwrap());
    }
}

#[test]
fn foreign_source_explain_uses_the_view_bound_namespace() {
    for provider in 0..4 {
        let directory = tempfile::tempdir().unwrap();
        let engine = open(provider, &directory.path().join("foreign-explain.db"));
        sql(&engine, "CREATE ROLE reader; CREATE SCHEMA hidden; CREATE TABLE hidden.local_rows(v integer); CREATE SERVER source FOREIGN DATA WRAPPER memory_fdw; CREATE FOREIGN TABLE hidden.remote(v integer) SERVER source; CREATE VIEW public.local_view AS SELECT * FROM hidden.local_rows; CREATE VIEW public.foreign_view AS SELECT * FROM hidden.remote; GRANT SELECT ON public.local_view, public.foreign_view TO reader; SET ROLE reader");
        sql(&engine, "EXPLAIN SELECT * FROM public.local_view");
        sql(&engine, "EXPLAIN SELECT * FROM public.foreign_view");
        let error = engine
            .sql("EXPLAIN SELECT * FROM hidden.remote", &[])
            .unwrap_err();
        assert_eq!(error.sqlstate(), Some("42501"));
    }
}

#[test]
fn foreign_table_server_reference_survives_concurrent_server_deletion() {
    let oracle: Value = serde_json::from_str(include_str!(
        "../../../../../../tests/parity/pg18/drop_foreign_server_reference_oracle.expected.json"
    ))
    .unwrap();
    for provider in 0..3 {
        for create_first in [true, false] {
            let (directory, first, second) = sessions(provider);
            for setup in oracle["setup"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|step| step["reference_only"] != true)
            {
                sql(&first, setup["sql"].as_str().unwrap());
            }
            let original = first.durable.foreign_servers.read()["competing"].clone();
            let original_oid = original.metadata.oid;
            let create = oracle["session_a"][1]["sql"].as_str().unwrap();
            let delete = oracle["session_b"]["sql"].as_str().unwrap();
            sql(&first, "BEGIN");
            sql(&first, if create_first { create } else { delete });
            sql(&second, "SET statement_timeout = '5s'");
            sql(&second, if create_first { delete } else { create });
            sql(&first, "COMMIT");
            drop(second);
            drop(first);
            let reopened = open(provider + 1, &directory.path().join("table-locks.db"));
            assert_eq!(
                reopened.list_foreign_tables().unwrap(),
                ["ds414_orphan.remote"]
            );
            let relation = uqa_core::RelationIdentity::new("ds414_orphan", "remote");
            for replace in [false, true] {
                if replace {
                    sql(&reopened, oracle["recreate"]["sql"].as_str().unwrap());
                    assert_ne!(
                        reopened.durable.foreign_servers.read()["competing"]
                            .metadata
                            .oid,
                        original_oid
                    );
                }
                let observer = reopened.new_session().unwrap();
                assert_eq!(
                    observer.durable.foreign_tables.read()[&relation].server_reference,
                    Some((&original).into())
                );
                let dependency = sql(&observer, "SELECT refobjid::text AS server_oid, deptype::text AS kind FROM pg_depend WHERE classid=1259 AND objid='ds414_orphan.remote'::regclass AND refclassid=1417");
                assert_eq!(dependency.rows.len(), 1);
                assert_eq!(
                    dependency.rows[0]["server_oid"],
                    uqa_core::Value::Str(original_oid.to_string())
                );
                assert_eq!(dependency.rows[0]["kind"], uqa_core::Value::Str("n".into()));
                let queries = if replace {
                    "query_after_recreate"
                } else {
                    "query_before_recreate"
                };
                for case in oracle[queries].as_array().unwrap() {
                    let query = case["sql"].as_str().unwrap();
                    let error = observer.sql(query, &[]).unwrap_err();
                    let expected: Value = serde_json::from_str(
                        &case["error"]
                            .to_string()
                            .replace("$server_oid", &original_oid.to_string()),
                    )
                    .unwrap();
                    assert_eq!(
                        json!({"severity":"ERROR", "sqlstate":error.sqlstate(), "message":error.to_string(), "detail":error.detail(), "hint":error.hint()}),
                        expected,
                        "{query}"
                    );
                }
                assert!(observer
                    .load_memory_foreign_table("ds414_orphan.remote", vec![])
                    .unwrap_err()
                    .contains(&format!(
                        "cache lookup failed for foreign server {original_oid}"
                    )));
            }
            sql(
                &reopened,
                oracle["drop_replacement_restrict"]["sql"].as_str().unwrap(),
            );
            sql(&reopened, oracle["drop_orphan"]["sql"].as_str().unwrap());
        }
    }
}

#[test]
fn foreign_table_server_reference_upgrade_is_atomic_initial_only_and_durable() {
    use std::sync::Arc;
    for provider in 1..4 {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("legacy-foreign-reference.db");
        let engine = open(provider, &path);
        sql(&engine, "CREATE SERVER source FOREIGN DATA WRAPPER memory_fdw; CREATE FOREIGN TABLE remote(v integer) SERVER source");
        let expected = engine.durable.foreign_servers.read()["source"].clone();
        let factory = Arc::clone(engine.storage.provider.as_ref().unwrap());
        let raw = factory.open_session().unwrap();
        let mut legacy_row = raw.catalog.load_foreign_tables().unwrap().remove(0);
        let mut schema: Value = serde_json::from_str(&legacy_row.columns_json).unwrap();
        schema["version"] = 1.into();
        schema.as_object_mut().unwrap().remove("server_reference");
        legacy_row.columns_json = schema.to_string();
        raw.backend.begin_transaction().unwrap();
        raw.catalog
            .delete_metadata("foreign-table-server-reference-format")
            .unwrap();
        raw.catalog.save_foreign_table(&legacy_row).unwrap();
        raw.backend.commit_transaction().unwrap();
        assert!(engine.new_session().is_err());
        raw.catalog.set_metadata("sql_triggers_json", "{").unwrap();
        drop(engine);
        assert!(Engine::from_persistent_provider(Arc::clone(&factory)).is_err());
        assert_eq!(
            raw.catalog.load_foreign_tables().unwrap()[0].columns_json,
            legacy_row.columns_json
        );
        assert!(raw
            .catalog
            .get_metadata("foreign-table-server-reference-format")
            .unwrap()
            .is_none());
        raw.catalog.delete_metadata("sql_triggers_json").unwrap();
        let upgraded = Engine::from_persistent_provider(Arc::clone(&factory)).unwrap();
        let relation = uqa_core::RelationIdentity::new("public", "remote");
        assert_eq!(
            upgraded.durable.foreign_tables.read()[&relation].server_reference,
            Some((&expected).into())
        );
        assert!(raw
            .catalog
            .get_metadata("foreign-table-server-reference-format")
            .unwrap()
            .is_some());
        let encoded = raw.catalog.load_foreign_tables().unwrap()[0].clone();
        assert_eq!(
            serde_json::from_str::<Value>(&encoded.columns_json).unwrap()["version"],
            2
        );
        drop(upgraded);
        // Missing identities in an already converted database are corruption, not a request to bind a new server by name.
        raw.catalog.save_foreign_table(&legacy_row).unwrap();
        assert!(Engine::from_persistent_provider(Arc::clone(&factory)).is_err());
        raw.catalog.save_foreign_table(&encoded).unwrap();
        drop(raw);
        drop(factory);
        let reopened = open(provider, &path);
        assert_eq!(
            reopened.durable.foreign_tables.read()[&relation].server_reference,
            Some((&expected).into())
        );
    }
}
