//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use std::sync::Arc;
use uqa_sql::catalog::foreign_server::{ForeignServerDefinition, ForeignServers};
use uqa_sql::catalog::roles::RoleDefinition;
use uqa_sql::catalog::roles::RoleDefinitions;
use uqa_sql::catalog::{
    foreign_server::ForeignServerMetadata,
    foreign_wrapper::{ForeignWrapperHandler, ForeignWrapperReference},
    roles::RoleIdentity,
};
use uqa_storage::{KeyValueCatalog, MemoryKeyValueStore};

fn roles() -> RoleDefinitions {
    RoleDefinitions::try_from_iter([("uqa".into(), RoleDefinition::bootstrap())]).unwrap()
}

fn catalog() -> KeyValueCatalog {
    KeyValueCatalog::new(Arc::new(MemoryKeyValueStore::new()))
}

fn restore(
    catalog: &dyn CatalogFacade,
    roles: &RoleDefinitions,
    servers: &mut ForeignServers,
    allow_migration: bool,
) -> StorageBackendResult<RestoredWrappers> {
    let budget = uqa_core::memory::MemoryBudget::new(1 << 20);
    let cancellation = uqa_core::CancellationToken::new();
    let control = uqa_core::memory::ProductionControl::new(&budget, &cancellation, &cancellation);
    super::restore(catalog, roles, servers, allow_migration, &control)
}

fn servers() -> ForeignServers {
    ForeignServers::try_from_iter([(
        "source".into(),
        ForeignServerDefinition {
            name: "source".into(),
            fdw_type: "memory_fdw".into(),
            options: uqa_core::memory::OwnedMap::new(),
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
    .unwrap()
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
        .edit("source", |server| {
            server.metadata.wrapper_reference = None;
            Ok(())
        })
        .unwrap()
        .unwrap();
    for migrate in [false, true] {
        assert!(restore(&catalog, &roles(), &mut servers.clone(), migrate).is_err());
    }
}

#[test]
fn failed_wrapper_migration_preserves_prior_servers_and_catalog_records() {
    let catalog = catalog();
    let mut servers = servers();
    let mut missing = servers["source"].clone();
    missing.name = "z_missing".into();
    missing.fdw_type = "absent".into();
    missing.metadata.oid += 1;
    missing.metadata.object_id = [2; 16];
    servers.insert(missing.name.clone(), missing).unwrap();
    let prior = servers.clone();
    let error = restore(&catalog, &roles(), &mut servers, true)
        .err()
        .unwrap();
    assert!(error.to_string().contains("missing wrapper"));
    assert_eq!(servers, prior);
    assert!(catalog.get_metadata(FORMAT_KEY).unwrap().is_none());
    assert!(catalog
        .metadata_with_prefix(RECORD_PREFIX)
        .unwrap()
        .is_empty());
}

#[test]
fn records_preserve_no_handler_and_option_order_without_touching_native_adapters() {
    let catalog = catalog();
    let mut servers = ForeignServers::new();
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
    for (name, native) in &native_wrappers() {
        assert_eq!(&restored.definitions[name], native);
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
        .edit("source", |server| {
            server
                .metadata
                .wrapper_reference
                .as_mut()
                .unwrap()
                .object_id[0] ^= 1;
            Ok(())
        })
        .unwrap()
        .unwrap();
    assert!(restore(&catalog, &roles(), &mut servers, true).is_err());
}

#[test]
fn live_wrapper_validation_distinguishes_invalid_and_reserved_references_from_retired_users() {
    use uqa_core::{
        memory::{MemoryBudget, ProductionControl},
        CancellationToken,
    };
    let mut servers = servers();
    let wrappers = native_wrappers();
    let native = wrappers["memory_fdw"].identity;
    let memory = MemoryBudget::new(4096);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&memory, &token, &token);
    for (identity, diagnostic) in [
        (None, Some("has no wrapper identity")),
        (
            Some(ForeignWrapperReference {
                oid: 16_384,
                object_id: [0; 16],
            }),
            Some("invalid stored foreign-wrapper reference"),
        ),
        (
            Some(ForeignWrapperReference { oid: 0, ..native }),
            Some("invalid stored foreign-wrapper reference"),
        ),
        (
            Some(ForeignWrapperReference {
                object_id: [4; 16],
                ..native
            }),
            Some("cache lookup failed for foreign-data wrapper"),
        ),
        (
            Some(ForeignWrapperReference {
                oid: 16_384,
                object_id: [4; 16],
            }),
            None,
        ),
        (Some(native), None),
    ] {
        servers
            .edit("source", |server| {
                server.metadata.wrapper_reference = identity;
                Ok(())
            })
            .unwrap()
            .unwrap();
        let result = crate::catalog::foreign::reference::validate_live_wrappers(
            &servers, &wrappers, &control,
        );
        if let Some(diagnostic) = diagnostic {
            assert!(result.unwrap_err().to_string().contains(diagnostic));
        } else {
            result.unwrap();
        }
        assert_eq!(memory.used(), 0);
    }
}

#[test]
fn partial_or_unknown_wrapper_formats_are_rejected_before_publication() {
    let catalog = catalog();
    let mut servers = ForeignServers::new();
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
    assert!(restore(&catalog, &RoleDefinitions::new(), &mut servers, true).is_err());
}

#[test]
fn native_wrapper_upgrade_is_initial_only_and_defers_all_writes() {
    let catalog = catalog();
    let mut servers = ForeignServers::new();
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
