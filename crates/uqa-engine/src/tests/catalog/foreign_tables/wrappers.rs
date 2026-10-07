//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::servers::open;
use crate::Engine;
use std::sync::Arc;
use uqa_sql::catalog::foreign_wrapper::{native_wrappers, ForeignWrapperHandler};

#[test]
fn foreign_option_order_upgrade_is_initial_only_and_rolls_back_with_catalog_restoration() {
    for provider in 1..4 {
        let directory = tempfile::tempdir().unwrap();
        let engine = open(
            provider,
            &directory.path().join("foreign-option-upgrade.db"),
        );
        engine.sql("CREATE SERVER source FOREIGN DATA WRAPPER memory_fdw; CREATE FOREIGN TABLE ordered_rows(id integer) SERVER source OPTIONS (z 'last',a 'first')", &[]).unwrap();
        let factory = Arc::clone(engine.storage.provider.as_ref().unwrap());
        let raw = factory.open_session().unwrap();
        let mut previous = raw.catalog.load_foreign_tables().unwrap().remove(0);
        let mut schema: serde_json::Value = serde_json::from_str(&previous.columns_json).unwrap();
        schema["version"] = 2.into();
        schema.as_object_mut().unwrap().remove("option_order");
        previous.columns_json = schema.to_string();
        raw.backend.begin_transaction().unwrap();
        raw.catalog.save_foreign_table(&previous).unwrap();
        raw.catalog
            .set_metadata("foreign-table-server-reference-format", r#"{"version":1}"#)
            .unwrap();
        raw.catalog.set_metadata("sql_triggers_json", "{").unwrap();
        raw.backend.commit_transaction().unwrap();
        assert!(engine.new_session().is_err());
        drop(engine);
        assert!(Engine::from_persistent_provider(Arc::clone(&factory)).is_err());
        assert_eq!(
            raw.catalog.load_foreign_tables().unwrap()[0].columns_json,
            previous.columns_json
        );
        assert_eq!(
            raw.catalog
                .get_metadata("foreign-table-server-reference-format")
                .unwrap()
                .as_deref(),
            Some(r#"{"version":1}"#)
        );
        raw.catalog.delete_metadata("sql_triggers_json").unwrap();
        let engine = Engine::from_persistent_provider(Arc::clone(&factory)).unwrap();
        let restored = raw.catalog.load_foreign_tables().unwrap().remove(0);
        let schema: serde_json::Value = serde_json::from_str(&restored.columns_json).unwrap();
        assert_eq!(schema["version"], 3);
        assert_eq!(schema["option_order"], serde_json::json!(["a", "z"]));
        assert_eq!(restored.options_json, previous.options_json);
        assert_eq!(
            raw.catalog
                .get_metadata("foreign-table-server-reference-format")
                .unwrap()
                .as_deref(),
            Some(r#"{"version":2}"#)
        );
        drop(engine);
        raw.catalog.save_foreign_table(&previous).unwrap();
        let error = Engine::from_persistent_provider(factory).err().unwrap();
        assert!(
            error
                .to_string()
                .contains("legacy schema under the current option-order marker"),
            "{error}"
        );
        assert_eq!(
            raw.catalog.load_foreign_tables().unwrap()[0].columns_json,
            previous.columns_json
        );
    }
}

pub(super) fn remove_wrapper_format(catalog: &dyn uqa_storage::CatalogFacade) {
    for (key, _) in catalog.metadata_with_prefix("foreign-wrapper/").unwrap() {
        catalog.delete_metadata(&key).unwrap();
    }
    catalog
        .delete_metadata("foreign-wrapper-catalog-format")
        .unwrap();
}

#[test]
fn server_options_keep_declaration_order_after_reopen() {
    for provider in 0..4 {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("ordered-options.db");
        let engine = open(provider, &path);
        engine.sql("CREATE SERVER ordered_options FOREIGN DATA WRAPPER memory_fdw OPTIONS (second '2', first '1')", &[]).unwrap();
        let engine = if provider == 0 {
            engine
        } else {
            drop(engine);
            open(provider, &path)
        };
        assert_eq!(
            engine.durable.foreign_servers.read()["ordered_options"]
                .metadata
                .option_order
                .as_deref(),
            Some(["second".to_owned(), "first".to_owned()].as_slice())
        );
    }
}

#[test]
fn server_validator_can_remove_its_wrapper_without_rebinding_the_published_reference() {
    for provider in 0..4 {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("orphaned-server.db");
        let engine = open(provider, &path);
        engine.sql("CREATE FUNCTION server_removes_wrapper(text[],oid) RETURNS integer LANGUAGE plpgsql AS $$ BEGIN IF $2=1417 THEN EXECUTE 'DROP FUNCTION server_removes_wrapper(text[],oid) CASCADE'; END IF; RETURN 1; END $$; CREATE FOREIGN DATA WRAPPER orphaned_fdw VALIDATOR server_removes_wrapper", &[]).unwrap();
        let original = engine.durable.foreign_wrappers.read()["orphaned_fdw"].identity;
        engine.sql("CREATE SERVER orphaned_server FOREIGN DATA WRAPPER orphaned_fdw; CREATE FOREIGN DATA WRAPPER orphaned_fdw", &[]).unwrap();
        let engine = if provider == 0 {
            engine
        } else {
            drop(engine);
            open(provider, &path)
        };
        assert_eq!(
            engine.durable.foreign_servers.read()["orphaned_server"]
                .metadata
                .wrapper_reference,
            Some(original)
        );
        assert_ne!(
            engine.durable.foreign_wrappers.read()["orphaned_fdw"].identity,
            original
        );
        let error = engine
            .sql(
                "CREATE FOREIGN TABLE orphaned_rows(id integer) SERVER orphaned_server",
                &[],
            )
            .unwrap_err();
        assert_eq!(error.sqlstate(), Some("XX000"));
        assert_eq!(
            error.to_string(),
            format!(
                "cache lookup failed for foreign-data wrapper {}",
                original.oid
            )
        );
    }
}

#[test]
fn a_self_removing_validator_keeps_its_original_oid_after_name_reuse_and_reopen() {
    for provider in 0..4 {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("orphan-validator.db");
        let engine = open(provider, &path);
        engine.sql("CREATE FUNCTION orphan_validator(text[],oid) RETURNS integer LANGUAGE plpgsql AS $$ BEGIN EXECUTE 'DROP FUNCTION orphan_validator(text[],oid)'; RETURN 1; END $$; CREATE FOREIGN DATA WRAPPER orphan_fdw VALIDATOR orphan_validator", &[]).unwrap();
        let original = engine.durable.foreign_wrappers.read()["orphan_fdw"]
            .validator
            .clone()
            .unwrap();
        engine.sql("CREATE FUNCTION orphan_validator(text[],oid) RETURNS integer LANGUAGE SQL RETURN 7", &[]).unwrap();
        let engine = if provider == 0 {
            engine
        } else {
            drop(engine);
            open(provider, &path)
        };
        assert_eq!(
            engine.durable.foreign_wrappers.read()["orphan_fdw"]
                .validator
                .as_ref(),
            Some(&original)
        );
        let error = engine
            .sql(
                "CREATE SERVER orphan_server FOREIGN DATA WRAPPER orphan_fdw",
                &[],
            )
            .unwrap_err();
        assert_eq!(error.sqlstate(), Some("XX000"));
        assert_eq!(
            error.to_string(),
            format!("cache lookup failed for function {}", original.oid)
        );
    }
}

#[test]
fn native_wrapper_references_survive_sessions_snapshots_and_reopen() {
    for provider in 0..4 {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("wrappers.db");
        let engine = open(provider, &path);
        engine
            .sql("CREATE SERVER source FOREIGN DATA WRAPPER memory_fdw", &[])
            .unwrap();
        let expected = engine.durable.foreign_servers.read()["source"]
            .metadata
            .wrapper_reference
            .unwrap();
        assert_eq!(expected, native_wrappers()["memory_fdw"].identity);
        let pinned = engine.catalog_read_view();
        assert_eq!(
            pinned.snapshot().definitions.foreign_wrappers["memory_fdw"].identity,
            expected
        );
        engine.sql("BEGIN; SAVEPOINT before_state", &[]).unwrap();
        engine.durable.foreign_wrappers.write().remove("memory_fdw");
        engine.sql("ROLLBACK TO before_state; COMMIT", &[]).unwrap();
        assert_eq!(
            engine.durable.foreign_wrappers.read()["memory_fdw"].identity,
            expected
        );
        assert_eq!(
            pinned.snapshot().definitions.foreign_wrappers["memory_fdw"].identity,
            expected
        );
        if provider > 0 {
            let peer = engine.new_session().unwrap();
            assert_eq!(*peer.durable.foreign_wrappers.read(), native_wrappers());
            drop(peer);
            drop(pinned);
            drop(engine);
            let engine = open(provider, &path);
            assert_eq!(
                engine.durable.foreign_servers.read()["source"]
                    .metadata
                    .wrapper_reference,
                Some(expected)
            );
        }
    }
}

#[test]
fn wrapper_restore_rejects_changed_references_and_does_not_repair_them() {
    for provider in 1..4 {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("wrapper-reference.db");
        let engine = open(provider, &path);
        engine
            .sql("CREATE SERVER source FOREIGN DATA WRAPPER memory_fdw", &[])
            .unwrap();
        let factory = Arc::clone(engine.storage.provider.as_ref().unwrap());
        let raw = factory.open_session().unwrap();
        let original = raw.catalog.load_foreign_server_rows().unwrap().remove(0);
        let mut changed = original.clone();
        let mut metadata: serde_json::Value =
            serde_json::from_str(changed.metadata_json.as_ref().unwrap()).unwrap();
        metadata["metadata"]["wrapper_reference"]["object_id"][0] = serde_json::json!(1);
        changed.metadata_json = Some(metadata.to_string());
        raw.catalog.save_foreign_server_row(&changed).unwrap();
        drop(engine);
        let error = Engine::from_persistent_provider(Arc::clone(&factory))
            .err()
            .unwrap();
        assert!(
            error
                .to_string()
                .contains("cache lookup failed for foreign-data wrapper"),
            "{error}"
        );
        assert_eq!(
            raw.catalog.load_foreign_server_rows().unwrap()[0].metadata_json,
            changed.metadata_json
        );
        raw.catalog.save_foreign_server_row(&original).unwrap();
        assert!(Engine::from_persistent_provider(factory).is_ok());
    }
}

#[test]
fn wrapper_upgrade_preserves_server_metadata_and_rolls_back_the_reader_fence() {
    for provider in 1..4 {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("wrapper-upgrade.db");
        let engine = open(provider, &path);
        engine.sql("CREATE ROLE owner; SET ROLE owner; CREATE SERVER source TYPE 'kind' VERSION 'v1' FOREIGN DATA WRAPPER memory_fdw OPTIONS (token 'retained'); RESET ROLE", &[]).unwrap();
        let expected = engine.durable.foreign_servers.read()["source"].clone();
        let factory = Arc::clone(engine.storage.provider.as_ref().unwrap());
        let raw = factory.open_session().unwrap();
        let mut previous = raw.catalog.load_foreign_server_rows().unwrap().remove(0);
        let mut metadata: serde_json::Value =
            serde_json::from_str(previous.metadata_json.as_ref().unwrap()).unwrap();
        metadata["metadata"]
            .as_object_mut()
            .unwrap()
            .remove("wrapper_reference");
        previous.metadata_json = Some(metadata.to_string());
        raw.backend.begin_transaction().unwrap();
        raw.catalog.save_foreign_server_row(&previous).unwrap();
        raw.catalog
            .set_metadata("foreign-server-metadata-format", r#"{"version":1}"#)
            .unwrap();
        remove_wrapper_format(raw.catalog.as_ref());
        raw.catalog.set_metadata("sql_triggers_json", "{").unwrap();
        raw.backend.commit_transaction().unwrap();
        assert!(engine.new_session().is_err());
        drop(engine);
        let error = Engine::from_persistent_provider(Arc::clone(&factory))
            .err()
            .unwrap();
        assert!(error.to_string().contains("EOF"), "{error}");
        assert_eq!(
            raw.catalog
                .get_metadata("foreign-server-metadata-format")
                .unwrap()
                .as_deref(),
            Some(r#"{"version":1}"#)
        );
        assert_eq!(
            raw.catalog.load_foreign_server_rows().unwrap()[0].metadata_json,
            previous.metadata_json
        );
        assert!(raw
            .catalog
            .get_metadata("foreign-wrapper-catalog-format")
            .unwrap()
            .is_none());
        assert!(raw
            .catalog
            .metadata_with_prefix("foreign-wrapper/")
            .unwrap()
            .is_empty());
        raw.catalog.delete_metadata("sql_triggers_json").unwrap();
        let engine = Engine::from_persistent_provider(factory).unwrap();
        assert_eq!(engine.durable.foreign_servers.read()["source"], expected);
        assert_eq!(
            raw.catalog
                .get_metadata("foreign-server-metadata-format")
                .unwrap()
                .as_deref(),
            Some(r#"{"version":2}"#)
        );
    }
}

#[test]
fn a_wrapper_without_a_handler_cannot_choose_a_native_adapter_by_name() {
    let engine = Engine::new();
    engine.sql("CREATE SERVER source FOREIGN DATA WRAPPER memory_fdw; CREATE FOREIGN TABLE items(a integer) SERVER source", &[]).unwrap();
    let reference = uqa_sql::catalog::foreign_wrapper::ForeignWrapperReference {
        oid: 16_384,
        object_id: [9; 16],
    };
    {
        let mut wrappers = engine.durable.foreign_wrappers.write();
        let wrapper = wrappers.get_mut("memory_fdw").unwrap();
        wrapper.handler = ForeignWrapperHandler::None;
        wrapper.identity = reference;
    }
    engine
        .durable
        .foreign_servers
        .write()
        .get_mut("source")
        .unwrap()
        .metadata
        .wrapper_reference = Some(reference);
    engine
        .sql("CREATE VIEW visible AS SELECT a FROM items", &[])
        .unwrap();
    assert!(engine
        .load_memory_foreign_table("items", Vec::new())
        .unwrap_err()
        .contains("has no handler"));
    for sql in [
        "SELECT * FROM items",
        "SELECT * FROM items LIMIT 0",
        "SELECT * FROM visible",
    ] {
        let error = engine.sql(sql, &[]).unwrap_err();
        assert_eq!(error.sqlstate(), Some("55000"), "{sql}: {error}");
        assert!(
            error
                .to_string()
                .contains("foreign-data wrapper \"memory_fdw\" has no handler"),
            "{error}"
        );
    }
}
