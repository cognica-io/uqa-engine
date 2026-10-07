//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use std::sync::Arc;
use uqa_sql::catalog::{
    foreign_server::ForeignServerMetadata,
    foreign_wrapper::{ForeignWrapperHandler, ForeignWrapperReference},
    roles::RoleIdentity,
};
use uqa_storage::{KeyValueCatalog, MemoryKeyValueStore};

fn roles() -> BTreeMap<String, RoleDefinition> {
    BTreeMap::from([("uqa".into(), RoleDefinition::bootstrap())])
}

fn catalog() -> KeyValueCatalog {
    KeyValueCatalog::new(Arc::new(MemoryKeyValueStore::new()))
}

fn servers() -> BTreeMap<String, ForeignServerDefinition> {
    BTreeMap::from([(
        "source".into(),
        ForeignServerDefinition {
            name: "source".into(),
            fdw_type: "memory_fdw".into(),
            options: BTreeMap::new(),
            metadata: ForeignServerMetadata {
                option_order: None,
                oid: 16_384,
                object_id: [1; 16],
                owner: RoleIdentity::BOOTSTRAP,
                server_type: None,
                version: None,
                wrapper_reference: None,
            },
        },
    )])
}

#[test]
fn initial_conversion_defers_writes_and_binds_each_server_once() {
    let catalog = catalog();
    let mut servers = servers();
    assert!(restore(&catalog, &roles(), &mut servers.clone(), false).is_err());
    let restored = restore(&catalog, &roles(), &mut servers, true).unwrap();
    assert_eq!(restored.definitions, native_wrappers());
    assert_eq!(
        catalog.metadata_with_prefix(RECORD_PREFIX).unwrap().len(),
        0
    );
    assert!(catalog.get_metadata(FORMAT_KEY).unwrap().is_none());
    assert_eq!(
        servers["source"]
            .bound_wrapper(&restored.definitions)
            .unwrap()
            .name,
        "memory_fdw"
    );
    restored.persist_migrations(&catalog).unwrap();
    let again = restore(&catalog, &roles(), &mut servers.clone(), false).unwrap();
    assert_eq!(again.definitions, restored.definitions);
    assert!(!again.initialize);
    assert_eq!(again.server_migrations.len(), 0);
    servers
        .get_mut("source")
        .unwrap()
        .metadata
        .wrapper_reference = None;
    for migrate in [false, true] {
        assert!(restore(&catalog, &roles(), &mut servers.clone(), migrate).is_err());
    }
}

#[test]
fn records_preserve_no_handler_and_option_order_without_touching_native_adapters() {
    let catalog = catalog();
    let mut servers = BTreeMap::new();
    restore(&catalog, &roles(), &mut servers, true)
        .unwrap()
        .persist_migrations(&catalog)
        .unwrap();
    let definition = ForeignWrapperDefinition {
        name: "custom/wrapper".into(),
        identity: ForeignWrapperReference {
            oid: 16_384,
            object_id: [7; 16],
        },
        owner: RoleIdentity::BOOTSTRAP,
        handler: ForeignWrapperHandler::None,
        validator: None,
        options: vec![("z".into(), "first".into()), ("a".into(), "second".into())],
    };
    persist(&catalog, &definition).unwrap();
    let restored = restore(&catalog, &roles(), &mut servers, false).unwrap();
    assert_eq!(restored.definitions["custom/wrapper"], definition);
    for (name, native) in native_wrappers() {
        assert_eq!(restored.definitions[&name], native);
    }
}

#[test]
fn current_catalog_never_repairs_missing_records_or_rebinds_an_incarnation() {
    let catalog = catalog();
    let mut servers = servers();
    restore(&catalog, &roles(), &mut servers, true)
        .unwrap()
        .persist_migrations(&catalog)
        .unwrap();
    let key = format!("{RECORD_PREFIX}memory_fdw");
    let original = catalog.get_metadata(&key).unwrap().unwrap();
    catalog.delete_metadata(&key).unwrap();
    assert!(restore(&catalog, &roles(), &mut servers.clone(), true).is_err());
    assert!(catalog.get_metadata(&key).unwrap().is_none());
    catalog.set_metadata(&key, &original).unwrap();
    servers
        .get_mut("source")
        .unwrap()
        .metadata
        .wrapper_reference
        .as_mut()
        .unwrap()
        .object_id[0] ^= 1;
    assert!(restore(&catalog, &roles(), &mut servers, true).is_err());
}

#[test]
fn partial_or_unknown_wrapper_formats_are_rejected_before_publication() {
    let catalog = catalog();
    let mut servers = BTreeMap::new();
    restore(&catalog, &roles(), &mut servers, true)
        .unwrap()
        .persist_migrations(&catalog)
        .unwrap();
    catalog.delete_metadata(FORMAT_KEY).unwrap();
    assert!(restore(&catalog, &roles(), &mut servers, true).is_err());
    catalog
        .set_metadata(FORMAT_KEY, r#"{"version":3}"#)
        .unwrap();
    assert!(restore(&catalog, &roles(), &mut servers, true).is_err());
    catalog.set_metadata(FORMAT_KEY, FORMAT_MARKER).unwrap();
    let key = format!("{RECORD_PREFIX}memory_fdw");
    let original = catalog.get_metadata(&key).unwrap().unwrap();
    for bad in [
        serde_json::json!({"version":3,"definition":native_wrappers()["memory_fdw"]}),
        serde_json::json!({"version":2,"definition":native_wrappers()["duckdb_fdw"]}),
        serde_json::json!({"version":2,"definition":native_wrappers()["memory_fdw"],"unknown":true}),
    ] {
        catalog.set_metadata(&key, &bad.to_string()).unwrap();
        assert!(restore(&catalog, &roles(), &mut servers, true).is_err());
    }
    catalog.set_metadata(&key, &original).unwrap();
    assert!(restore(&catalog, &BTreeMap::new(), &mut servers, true).is_err());
}

#[test]
fn native_wrapper_upgrade_is_initial_only_and_defers_all_writes() {
    let catalog = catalog();
    let mut servers = BTreeMap::new();
    catalog
        .set_metadata(FORMAT_KEY, r#"{"version":1}"#)
        .unwrap();
    for definition in native_wrappers().values() {
        catalog
            .set_metadata(
                &format!("{RECORD_PREFIX}{}", definition.name),
                &serde_json::json!({"version":1,"definition":definition}).to_string(),
            )
            .unwrap();
    }
    let before = catalog.metadata_with_prefix(RECORD_PREFIX).unwrap();
    assert!(restore(&catalog, &roles(), &mut servers, false).is_err());
    let upgraded = restore(&catalog, &roles(), &mut servers, true).unwrap();
    assert!(upgraded.upgrade);
    assert_eq!(catalog.metadata_with_prefix(RECORD_PREFIX).unwrap(), before);
    assert_eq!(
        catalog.get_metadata(FORMAT_KEY).unwrap().as_deref(),
        Some(r#"{"version":1}"#)
    );
    upgraded.persist_migrations(&catalog).unwrap();
    assert_eq!(
        catalog.get_metadata(FORMAT_KEY).unwrap().as_deref(),
        Some(FORMAT_MARKER)
    );
    assert_eq!(
        restore(&catalog, &roles(), &mut servers, false)
            .unwrap()
            .definitions,
        native_wrappers()
    );
}
