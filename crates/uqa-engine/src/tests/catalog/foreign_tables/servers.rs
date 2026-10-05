//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Server ownership, identity and retained catalog snapshots across transaction and provider boundaries.

use crate::Engine;
use std::{path::Path, sync::Arc};
use uqa_sql::catalog::{foreign_server::ForeignServerDefinition, roles::RoleIdentity};

pub(super) fn open(provider: usize, path: &Path) -> Engine {
    match provider {
        0 => Engine::new(),
        1 => Engine::open(path).unwrap(),
        2 => Engine::from_persistent_provider(Arc::new(
            uqa_storage_sqlite::SQLiteKeyValueStorage::open(path).unwrap(),
        ))
        .unwrap(),
        3 => Engine::from_persistent_provider(Arc::new(
            uqa_storage_redb::RedbStorage::open(path).unwrap(),
        ))
        .unwrap(),
        _ => unreachable!(),
    }
}
fn sql(engine: &Engine, statement: &str) {
    engine
        .sql(statement, &[])
        .unwrap_or_else(|error| panic!("{statement}: {error}"));
}
fn server(engine: &Engine, name: &str) -> ForeignServerDefinition {
    engine.durable.foreign_servers.read()[name].clone()
}

#[test]
fn foreign_server_owner_and_identity_survive_rename_undo_refresh_and_reopen() {
    for provider in 0..4 {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("servers.db");
        let engine = open(provider, &path);
        sql(&engine, "CREATE ROLE server_owner; SET ROLE server_owner; CREATE SERVER source TYPE '' VERSION '' FOREIGN DATA WRAPPER memory_fdw OPTIONS (metadata_json 'connection option'); RESET ROLE");
        let expected = server(&engine, "source");
        assert_eq!(expected.metadata.server_type.as_deref(), Some(""));
        assert_eq!(expected.metadata.version.as_deref(), Some(""));
        let owner = engine.durable.roles.read()["server_owner"].clone();
        assert_eq!(
            expected.metadata.owner,
            RoleIdentity {
                oid: owner.oid,
                object_id: owner.object_id
            }
        );
        assert_eq!(expected.options["metadata_json"], "connection option");
        assert_eq!(
            engine.foreign_server("source").unwrap().unwrap().options,
            expected.options
        );
        let retained = engine.durable.foreign_servers.snapshot();
        let pinned = engine.catalog_read_view();
        sql(&engine, "ALTER ROLE server_owner RENAME TO renamed_owner; BEGIN; SAVEPOINT before_server; CREATE SERVER transient FOREIGN DATA WRAPPER memory_fdw");
        assert_eq!(retained.len(), 1);
        assert_eq!(pinned.snapshot().definitions.foreign_servers.len(), 1);
        let transient = server(&engine, "transient");
        assert_ne!(transient.metadata.oid, expected.metadata.oid);
        assert_ne!(transient.metadata.object_id, expected.metadata.object_id);
        sql(&engine, "ROLLBACK TO before_server; COMMIT");
        assert!(engine.foreign_server("transient").unwrap().is_none());
        assert_eq!(server(&engine, "source"), expected);
        sql(
            &engine,
            "CREATE SERVER transient FOREIGN DATA WRAPPER memory_fdw",
        );
        assert_ne!(
            server(&engine, "transient").metadata.object_id,
            transient.metadata.object_id
        );
        assert_ne!(
            server(&engine, "transient").metadata.oid,
            transient.metadata.oid
        );
        assert_eq!(
            engine
                .sql("DROP ROLE renamed_owner", &[])
                .unwrap_err()
                .sqlstate(),
            Some("2BP01")
        );
        if provider > 0 {
            let peer = engine.new_session().unwrap();
            sql(
                &peer,
                "CREATE SERVER peer_server FOREIGN DATA WRAPPER memory_fdw",
            );
            assert!(engine.foreign_server("peer_server").unwrap().is_some());
            assert_eq!(server(&engine, "source"), expected);
            drop(peer);
            drop(pinned);
            drop(engine);
            let reopened = open(provider, &path);
            assert_eq!(server(&reopened, "source"), expected);
            assert_eq!(
                reopened
                    .sql("DROP ROLE renamed_owner", &[])
                    .unwrap_err()
                    .sqlstate(),
                Some("2BP01")
            );
        }
    }
}

#[test]
fn foreign_server_initial_open_upgrades_legacy_metadata_once_for_each_persistent_provider() {
    for provider in 1..4 {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("legacy-server.db");
        let engine = open(provider, &path);
        let factory = Arc::clone(engine.storage.provider.as_ref().unwrap());
        let raw = factory.open_session().unwrap();
        raw.backend.begin_transaction().unwrap();
        raw.catalog
            .delete_metadata("foreign-server-metadata-format")
            .unwrap();
        raw.catalog
            .save_foreign_server("legacy", "memory_fdw", r#"{"key":"original"}"#)
            .unwrap();
        raw.backend.commit_transaction().unwrap();
        assert!(
            engine.new_session().is_err(),
            "refresh must not migrate legacy metadata"
        );
        raw.catalog.set_metadata("sql_triggers_json", "{").unwrap();
        drop(engine);
        let failure = Engine::from_persistent_provider(Arc::clone(&factory))
            .err()
            .expect("malformed later catalog must reject open");
        assert!(failure.to_string().contains("EOF"), "{failure}");
        assert!(raw
            .catalog
            .get_metadata("foreign-server-metadata-format")
            .unwrap()
            .is_none());
        assert!(raw.catalog.load_foreign_server_rows().unwrap()[0]
            .metadata_json
            .is_none());
        raw.catalog.delete_metadata("sql_triggers_json").unwrap();
        let upgraded = Engine::from_persistent_provider(Arc::clone(&factory)).unwrap();
        let expected = server(&upgraded, "legacy");
        assert_eq!(expected.metadata.owner, RoleIdentity::BOOTSTRAP);
        assert!(expected.metadata.oid >= 16_384);
        assert_ne!(expected.metadata.object_id, [0; 16]);
        assert_eq!(expected.options["key"], "original");
        assert!(upgraded
            .storage
            .catalog
            .as_ref()
            .unwrap()
            .load_foreign_server_rows()
            .unwrap()[0]
            .metadata_json
            .is_some());
        drop(upgraded);
        drop(raw);
        drop(factory);
        let reopened = open(provider, &path);
        assert_eq!(server(&reopened, "legacy"), expected);
    }
}

#[test]
fn foreign_server_direct_options_remain_maps_and_empty_names_never_publish() {
    let engine = Engine::new();
    engine
        .register_foreign_server(
            "source".into(),
            "memory_fdw".into(),
            vec![
                ("a=b".into(), "first".into()),
                ("a=b".into(), "last".into()),
            ],
            false,
        )
        .unwrap();
    assert_eq!(
        engine.foreign_server("source").unwrap().unwrap().options["a=b"],
        "last"
    );
    assert!(engine
        .register_foreign_server(String::new(), "memory_fdw".into(), vec![], false)
        .is_err());
    assert_eq!(engine.list_foreign_servers().unwrap(), ["source"]);
}

#[test]
fn foreign_server_creation_retains_owner_until_commit_or_rollback() {
    use crate::tests::relation_lock_support::{after_shared_wait, sessions};
    use uqa_execution::row_locks::shared_objects::SharedCatalogLock;
    for provider in 0..3 {
        for finish in ["COMMIT", "ROLLBACK"] {
            let (_directory, first, second) = sessions(provider);
            sql(&first, "CREATE ROLE server_owner");
            sql(&first, "BEGIN; SET LOCAL ROLE server_owner; CREATE SERVER owned FOREIGN DATA WRAPPER memory_fdw");
            let oid = u32::try_from(first.durable.roles.read()["server_owner"].oid).unwrap();
            let (_second, result) = after_shared_wait(
                &first,
                second,
                "DROP ROLE server_owner",
                SharedCatalogLock::Object {
                    class_id: 1260,
                    oid,
                },
                finish,
            );
            if finish == "COMMIT" {
                assert_eq!(result.unwrap_err().sqlstate(), Some("2BP01"));
            } else {
                result.unwrap();
            }
        }
    }
}

#[test]
fn foreign_server_name_reservations_follow_competing_commit_or_rollback() {
    use crate::tests::relation_lock_support::{after_shared_wait, sessions};
    use uqa_execution::row_locks::shared_objects::SharedCatalogLock;
    let oracle: serde_json::Value = serde_json::from_str(include_str!(
        "../../../../../../tests/parity/pg18/foreign_server_concurrency_oracle.expected.json"
    ))
    .unwrap();
    for provider in 0..3 {
        for schedule in oracle["schedules"].as_array().unwrap() {
            let finish = schedule["finish_a"].as_str().unwrap();
            let (_directory, first, second) = sessions(provider);
            for statement in schedule["session_a"].as_array().unwrap() {
                sql(&first, statement.as_str().unwrap());
            }
            let original = server(&first, "competing");
            let (second, result) = after_shared_wait(
                &first,
                second,
                schedule["session_b"].as_str().unwrap(),
                SharedCatalogLock::Name {
                    class_id: 1417,
                    name: "competing",
                },
                finish,
            );
            if schedule["error"].is_null() {
                let result = result.unwrap();
                assert_eq!(
                    result.command_tag.as_deref(),
                    schedule["command_tag"].as_str()
                );
                assert_ne!(
                    server(&second, "competing").metadata.object_id,
                    original.metadata.object_id
                );
            } else {
                let error = result.unwrap_err();
                assert_eq!(
                    serde_json::json!({"sqlstate":error.sqlstate(),"message":error.to_string(),"detail":error.detail(),"hint":error.hint()}),
                    schedule["error"]
                );
                assert_eq!(server(&first, "competing"), original);
            }
            assert!(second.take_sql_notices().is_empty());
            assert_eq!(
                second.foreign_server("competing").unwrap().unwrap().options["source"],
                schedule["final_source"]
            );
        }
    }
}
