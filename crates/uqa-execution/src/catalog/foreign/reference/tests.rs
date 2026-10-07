//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_sql::catalog::{foreign_server::ForeignServerMetadata, roles::RoleIdentity};

fn definition() -> ForeignServerDefinition {
    ForeignServerDefinition {
        name: "source".into(),
        fdw_type: "memory_fdw".into(),
        options: BTreeMap::new(),
        metadata: ForeignServerMetadata {
            option_order: None,
            wrapper_reference: None,
            oid: 16_384,
            object_id: [1; 16],
            owner: RoleIdentity::BOOTSTRAP,
            server_type: None,
            version: None,
        },
    }
}

fn table() -> StoredForeignTable {
    StoredForeignTable::from_catalog(
        "remote".into(),
        "source".into(),
        BTreeMap::new(),
        r#"{"version":1,"object_id":[2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2],"columns":[]}"#,
    )
    .unwrap()
    .0
}

#[test]
fn legacy_server_names_bind_only_during_initial_restoration() {
    let servers = BTreeMap::from([("source".into(), definition())]);
    let mut table = table();
    assert!(restore(&mut table, &servers, false, false).is_err());
    assert!(restore(&mut table, &servers, true, true).is_err());
    assert!(restore(&mut table, &BTreeMap::new(), false, true).is_err());
    assert!(table.server_reference.is_none());
    assert!(restore(&mut table, &servers, false, true).unwrap());
    assert_eq!(
        table.server_reference,
        Some(ForeignServerReference::from(&servers["source"]))
    );
    assert!(!restore(&mut table, &servers, true, false).unwrap());
}

#[test]
fn current_server_references_do_not_rebind_absent_or_recreated_servers() {
    let mut table = table();
    let original = definition();
    table.server_reference = Some(ForeignServerReference::from(&original));
    let pinned = BTreeMap::from([("source".into(), original.clone())]);
    assert_eq!(table.bound_server(&pinned).unwrap(), &original);
    let mut replacement = original;
    replacement.metadata.object_id = [3; 16];
    for servers in [
        BTreeMap::new(),
        BTreeMap::from([("source".into(), replacement)]),
    ] {
        assert!(!restore(&mut table, &servers, true, false).unwrap());
        let error = table.bound_server(&servers).unwrap_err();
        assert_eq!(error.sqlstate(), Some("XX000"));
        assert_eq!(
            error.to_string(),
            "cache lookup failed for foreign server 16384"
        );
        assert_eq!(error.detail(), None);
        assert_eq!(error.hint(), None);
    }
    assert!(table.bound_server(&pinned).is_ok());
}

#[test]
fn current_foreign_schema_requires_a_valid_versioned_reference() {
    let mut table = table();
    table.server_reference = Some(ForeignServerReference::from(&definition()));
    let encoded = table.schema_json().unwrap();
    let (restored, legacy) = StoredForeignTable::from_catalog(
        table.name.clone(),
        table.server_name.clone(),
        table.options.clone(),
        &encoded,
    )
    .unwrap();
    assert!(!legacy);
    assert_eq!(restored.server_reference, table.server_reference);
    for (version, reference) in [
        (2, None),
        (1, table.server_reference),
        (
            2,
            Some(ForeignServerReference {
                oid: 0,
                object_id: [1; 16],
            }),
        ),
        (
            2,
            Some(ForeignServerReference {
                oid: 16_384,
                object_id: [0; 16],
            }),
        ),
    ] {
        let mut corrupt: serde_json::Value = serde_json::from_str(&encoded).unwrap();
        corrupt["version"] = version.into();
        corrupt["server_reference"] = serde_json::to_value(reference).unwrap();
        assert!(StoredForeignTable::from_catalog(
            "remote".into(),
            "source".into(),
            BTreeMap::new(),
            &corrupt.to_string()
        )
        .is_err());
    }
    assert!(restore(&mut table, &BTreeMap::new(), false, true).is_err());
}

#[test]
fn reference_format_marker_rejects_unknown_formats() {
    let catalog = uqa_storage::KeyValueCatalog::new(std::sync::Arc::new(
        uqa_storage::MemoryKeyValueStore::new(),
    ));
    assert_eq!(check_format(&catalog).unwrap(), None);
    initialize_format(&catalog).unwrap();
    assert_eq!(check_format(&catalog).unwrap(), Some(2));
    catalog
        .set_metadata(FORMAT_KEY, r#"{"version":99}"#)
        .unwrap();
    assert!(check_format(&catalog).is_err());
}

#[test]
fn foreign_option_order_round_trips_and_legacy_maps_convert_without_inventing_history() {
    let mut table = table();
    table.server_reference = Some(ForeignServerReference::from(&definition()));
    table.options = BTreeMap::from([("a".into(), "first".into()), ("z".into(), "last".into())]);
    table.option_order = vec!["z".into(), "a".into()];
    let encoded = table.schema_json().unwrap();
    let read = |encoded: &str| {
        StoredForeignTable::from_catalog(
            table.name.clone(),
            table.server_name.clone(),
            table.options.clone(),
            encoded,
        )
    };
    let (restored, legacy) = read(&encoded).unwrap();
    assert!(!legacy);
    assert_eq!(restored.option_order, table.option_order);
    for order in [
        serde_json::json!(["a", "a"]),
        serde_json::json!(["a"]),
        serde_json::json!(["a", "missing"]),
        serde_json::Value::Null,
    ] {
        let mut invalid: serde_json::Value = serde_json::from_str(&encoded).unwrap();
        invalid["option_order"] = order;
        assert!(read(&invalid.to_string()).is_err());
    }
    let mut previous: serde_json::Value = serde_json::from_str(&encoded).unwrap();
    previous["version"] = 2.into();
    previous.as_object_mut().unwrap().remove("option_order");
    let (converted, legacy) = read(&previous.to_string()).unwrap();
    assert!(legacy);
    assert_eq!(converted.option_order, vec!["a", "z"]);
    assert_eq!(converted.options, table.options);
    assert_eq!(converted.server_reference, table.server_reference);
    assert!(validate_schema_format(Some(2), legacy, &table.name).is_err());
}

#[test]
fn preceding_reference_marker_requires_initial_conversion_without_early_publication() {
    let catalog = uqa_storage::KeyValueCatalog::new(std::sync::Arc::new(
        uqa_storage::MemoryKeyValueStore::new(),
    ));
    catalog
        .set_metadata(FORMAT_KEY, r#"{"version":1}"#)
        .unwrap();
    assert!(restore_format(&catalog, false).is_err());
    assert_eq!(restore_format(&catalog, true).unwrap(), Some(1));
    assert_eq!(check_format(&catalog).unwrap(), Some(1));
    initialize_format(&catalog).unwrap();
    assert_eq!(restore_format(&catalog, false).unwrap(), Some(2));
}
