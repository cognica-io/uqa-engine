//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use std::sync::Arc;
use uqa_storage::{KeyValueCatalog, MemoryKeyValueStore};

fn roles() -> BTreeMap<String, RoleDefinition> {
    BTreeMap::from([("uqa".into(), RoleDefinition::bootstrap())])
}
fn catalog() -> KeyValueCatalog {
    KeyValueCatalog::new(Arc::new(MemoryKeyValueStore::new()))
}
fn definition() -> ForeignServerDefinition {
    ForeignServerDefinition {
        name: "source".into(),
        fdw_type: "memory_fdw".into(),
        options: BTreeMap::from([("metadata_json".into(), "user option".into())]),
        metadata: ForeignServerMetadata {
            oid: 16_384,
            object_id: [1; 16],
            owner: RoleIdentity::BOOTSTRAP,
            server_type: Some(String::new()),
            version: None,
        },
    }
}

#[test]
fn foreign_server_roundtrip_preserves_identity_and_does_not_use_connection_options() {
    let catalog = catalog();
    let expected = definition();
    persist(&catalog, &expected).unwrap();
    let actual = restore(&catalog, &roles(), false).unwrap();
    assert_eq!(actual.definitions["source"], expected);
    assert_eq!(
        fdw_definition(&actual.definitions["source"]).options,
        expected.options
    );
    // A legacy provider caller updates connection settings without discarding ownership.
    catalog
        .save_foreign_server("source", "memory_fdw", "{}")
        .unwrap();
    let actual = restore(&catalog, &roles(), false).unwrap();
    assert_eq!(actual.definitions["source"].metadata, expected.metadata);
    assert!(actual.definitions["source"].options.is_empty());
}

#[test]
fn foreign_server_legacy_upgrade_is_deferred_until_validation_and_only_allowed_on_initial_open() {
    let catalog = catalog();
    catalog
        .save_foreign_server("source", "memory_fdw", r#"{"source":"preserved"}"#)
        .unwrap();
    assert!(restore(&catalog, &roles(), false).is_err());
    let restored = restore(&catalog, &roles(), true).unwrap();
    let server = &restored.definitions["source"];
    assert_eq!(server.metadata.owner, RoleIdentity::BOOTSTRAP);
    assert_eq!(server.options["source"], "preserved");
    assert!(catalog.load_foreign_server_rows().unwrap()[0]
        .metadata_json
        .is_none());
    assert!(catalog.get_metadata(FORMAT_KEY).unwrap().is_none());
    restored.persist_migrations(&catalog).unwrap();
    assert_eq!(
        restore(&catalog, &roles(), false).unwrap().definitions,
        restored.definitions
    );
    catalog
        .save_foreign_server("unversioned", "memory_fdw", "{}")
        .unwrap();
    for allow_migration in [false, true] {
        assert!(restore(&catalog, &roles(), allow_migration).is_err());
    }
}

#[test]
fn foreign_server_restore_rejects_bad_envelopes_and_dangling_role_incarnations() {
    let catalog = catalog();
    let expected = definition();
    persist(&catalog, &expected).unwrap();
    let original = catalog_row(&expected).unwrap();
    let good: serde_json::Value =
        serde_json::from_str(original.metadata_json.as_ref().unwrap()).unwrap();
    let mut corruptions = vec![
        serde_json::Value::Null,
        serde_json::json!({"format_version": 1}),
    ];
    for (path, value) in [
        ("format_version", serde_json::json!(2)),
        ("unknown", serde_json::json!(true)),
    ] {
        let mut bad = good.clone();
        bad[path] = value;
        corruptions.push(bad);
    }
    for (path, value) in [
        ("oid", serde_json::json!(0)),
        ("object_id", serde_json::json!(vec![0; 16])),
        (
            "owner",
            serde_json::json!({"oid": 10, "object_id": vec![2; 16]}),
        ),
    ] {
        let mut bad = good.clone();
        bad["metadata"][path] = value;
        corruptions.push(bad);
    }
    for bad in corruptions {
        let mut row = original.clone();
        row.metadata_json = Some(bad.to_string());
        catalog.save_foreign_server_row(&row).unwrap();
        for allow_migration in [false, true] {
            assert!(
                restore(&catalog, &roles(), allow_migration).is_err(),
                "{bad}"
            );
        }
    }
    catalog.save_foreign_server_row(&original).unwrap();
    for marker in [
        "{}",
        "null",
        r#"{"version":2}"#,
        r#"{"version":1,"extra":true}"#,
    ] {
        catalog.set_metadata(FORMAT_KEY, marker).unwrap();
        assert!(restore(&catalog, &roles(), true).is_err(), "{marker}");
    }
    catalog.delete_metadata(FORMAT_KEY).unwrap();
    assert!(restore(&catalog, &roles(), true).is_err());
}

#[test]
fn foreign_server_restore_rejects_oid_and_incarnation_aliases() {
    for duplicate_oid in [false, true] {
        let catalog = catalog();
        let first = definition();
        let mut second = first.clone();
        second.name = "another".into();
        if duplicate_oid {
            second.metadata.object_id = [2; 16];
        } else {
            second.metadata.oid += 1;
        }
        persist(&catalog, &first).unwrap();
        persist(&catalog, &second).unwrap();
        assert!(restore(&catalog, &roles(), true).is_err());
    }
}
