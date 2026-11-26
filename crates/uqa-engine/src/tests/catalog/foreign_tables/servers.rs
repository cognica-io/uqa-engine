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
            engine
                .foreign_server_definition("source")
                .unwrap()
                .unwrap()
                .options,
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
        assert!(engine
            .foreign_server_definition("transient")
            .unwrap()
            .is_none());
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
            assert!(engine
                .foreign_server_definition("peer_server")
                .unwrap()
                .is_some());
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
        super::wrappers::remove_wrapper_format(raw.catalog.as_ref());
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
        assert!(raw
            .catalog
            .get_metadata("foreign-wrapper-catalog-format")
            .unwrap()
            .is_none());
        assert_eq!(
            raw.catalog
                .metadata_with_prefix("foreign-wrapper/")
                .unwrap()
                .len(),
            0
        );
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
        engine
            .foreign_server_definition("source")
            .unwrap()
            .unwrap()
            .options["a=b"],
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
            assert_eq!(second.take_sql_notices().len(), 0);
            assert_eq!(
                second
                    .foreign_server_definition("competing")
                    .unwrap()
                    .unwrap()
                    .options["source"],
                schedule["final_source"]
            );
        }
    }
}

#[rstest::rstest]
#[case::sqlite(0)]
#[case::sqlite_key_value(1)]
#[case::redb(2)]
fn alter_server_writers_match_postgresql_tuple_waits(#[case] provider: usize) {
    use crate::tests::relation_lock_support::{after_shared_wait_with_release, sessions};
    use uqa_execution::row_locks::shared_objects::SharedCatalogLock;
    let oracle: serde_json::Value = serde_json::from_str(include_str!(
        "../../../../../../tests/parity/pg18/alter_foreign_server_concurrency.expected.json"
    ))
    .unwrap();
    let (_directory, first, mut peer) = sessions(provider);
    sql(&first, "CREATE FOREIGN DATA WRAPPER concurrent_fdw");
    for schedule in oracle["schedules"].as_array().unwrap() {
        sql(
            &first,
            "CREATE SERVER changed FOREIGN DATA WRAPPER concurrent_fdw OPTIONS (k 'old')",
        );
        let oid = server(&first, "changed").metadata.oid;
        sql(&first, "BEGIN");
        sql(&first, schedule["first"].as_str().unwrap());
        let waited = SharedCatalogLock::Tuple {
            class_id: uqa_sql::catalog::dependencies::FOREIGN_SERVER_CLASS,
            oid,
        };
        let (returned, result) = after_shared_wait_with_release(
            &first,
            peer,
            schedule["second"].as_str().unwrap(),
            waited,
            || first.sql(schedule["finish"].as_str().unwrap(), &[]),
        );
        peer = returned;
        if let Some(state) = schedule["result"]["state"].as_str() {
            let error = result.unwrap_err();
            assert_eq!(error.sqlstate(), Some(state), "{schedule}");
            assert_eq!(
                error.to_string(),
                schedule["result"]["message"].as_str().unwrap(),
                "{schedule}"
            );
        } else {
            assert_eq!(
                result.unwrap().command_tag.as_deref(),
                schedule["result"]["tag"].as_str(),
                "{schedule}"
            );
        }
        let rows = peer.sql("SELECT srvoptions::text AS options FROM pg_foreign_server WHERE srvname = 'changed'", &[]).unwrap().rows;
        let expected = schedule["final"].as_array().unwrap();
        assert_eq!(rows.len(), expected.len(), "{schedule}");
        for (row, expected) in rows.iter().zip(expected) {
            let option = &expected[0];
            let value = option.as_str().map_or(uqa_core::Value::Null, |text| {
                uqa_core::Value::Str(text.into())
            });
            assert_eq!(row["options"], value, "{schedule}");
        }
        sql(&first, "DROP SERVER IF EXISTS changed");
    }
}

#[rstest::rstest]
#[case::memory(0)]
#[case::sqlite(1)]
#[case::sqlite_key_value(2)]
#[case::redb(3)]
fn altered_server_retains_bound_definitions(#[case] provider: usize) {
    let directory = tempfile::tempdir().unwrap();
    let engine = open(provider, &directory.path().join("retained.db"));
    sql(&engine, "CREATE SERVER source FOREIGN DATA WRAPPER memory_fdw OPTIONS (z 'old'); CREATE FOREIGN TABLE foreign_rows(v integer) SERVER source");
    let before = engine.durable.foreign_servers.snapshot();
    let view = engine.catalog_read_view();
    sql(&engine, "ALTER SERVER source OPTIONS (SET z 'new')");
    let current = server(&engine, "source");
    let original = &before["source"];
    assert_eq!(current.metadata.oid, original.metadata.oid);
    assert_eq!(current.metadata.object_id, original.metadata.object_id);
    assert_eq!(
        current.metadata.wrapper_reference,
        original.metadata.wrapper_reference
    );
    assert_eq!(current.metadata.owner, original.metadata.owner);
    assert_eq!(current.options["z"], "new");
    assert_eq!(original.options["z"], "old");
    assert_eq!(
        view.snapshot()
            .definitions
            .foreign_servers
            .get("source")
            .unwrap(),
        original
    );
}
