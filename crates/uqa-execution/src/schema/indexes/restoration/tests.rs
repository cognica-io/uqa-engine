//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::catalog::{
    security::BoundTableSecurity, test_support::empty_catalog, CatalogTableSnapshot,
    RelationLookupMode,
};
use std::sync::Arc;
use uqa_sql::{
    ast::{RelationPersistence, TableHierarchy},
    catalog::index::IndexDefinition,
};
use uqa_storage::{KeyValueCatalog, MemoryKeyValueStore, RelationSecurityRow, TableSchema};

fn fixture() -> (KeyValueCatalog, CatalogReadView, RelationNameResolution) {
    let storage = KeyValueCatalog::new(Arc::new(MemoryKeyValueStore::new()));
    storage
        .save_schema_row(&uqa_storage::SchemaRow::bootstrap("public"))
        .unwrap();
    let relation = RelationIdentity::new("public", "t");
    storage
        .save_table(&TableSchema {
            relation: relation.clone(),
            security: RelationSecurityRow::legacy("uqa"),
            object_id: [1; 16],
            storage_generation: [2; 16],
            analyzer_json: "{}".into(),
            fts_fields: Vec::new(),
            vector_fields: Vec::new(),
            columns_json: "[]".into(),
            constraints_json: "{}".into(),
        })
        .unwrap();
    let mut snapshot = empty_catalog().snapshot().clone();
    snapshot.tables.insert(
        relation,
        CatalogTableSnapshot {
            object_id: [1; 16],
            security: Arc::new(BoundTableSecurity::owner(
                uqa_sql::catalog::roles::RoleIdentity::BOOTSTRAP,
            )),
            columns: Arc::default(),
            columns_declared: true,
            checks: Arc::default(),
            foreign_keys: Arc::default(),
            keys: Arc::default(),
            hierarchy: Arc::new(TableHierarchy::default()),
            persistence: RelationPersistence::Permanent,
        },
    );
    let resolution = RelationNameResolution {
        search_path: Vec::new(),
        temporary_schema: "pg_temp_1".into(),
        temporary_namespace_allocated: false,
        current_user: "restricted".into(),
        lookup_mode: RelationLookupMode::Bound,
    };
    (storage, CatalogReadView::new(snapshot), resolution)
}

fn row(name: &str) -> CatalogIndexRow {
    CatalogIndexRow {
        relation: RelationIdentity::new("public", name),
        table_name: "public.t".into(),
        index_type: "btree".into(),
        columns_json: "[]".into(),
        parameters_json: "{}".into(),
        definition_json: None,
    }
}

#[test]
fn legacy_conversion_preserves_oid_and_physical_namespace_and_becomes_load_only() {
    let (storage, catalog, resolution) = fixture();
    storage.save_catalog_index_row(&row("legacy")).unwrap();
    let rows = restore(&storage, &catalog, &resolution, true).unwrap();
    let definition = crate::catalog::index::index_definition(&rows[0]).unwrap();
    let identity = definition.catalog.unwrap();
    assert_eq!(
        identity.identity.oid,
        uqa_sql::catalog::oids::relation_oid("i", "public", "legacy")
    );
    assert_eq!(identity.table_object_id, [1; 16]);
    assert_eq!(identity.physical_key, "public.legacy");
    assert_ne!(identity.identity.object_id, [0; 16]);
    assert_eq!(
        restore(&storage, &catalog, &resolution, false).unwrap()[0].definition_json,
        rows[0].definition_json
    );
    storage.save_catalog_index_row(&row("legacy")).unwrap();
    for allow_migration in [false, true] {
        assert!(restore(&storage, &catalog, &resolution, allow_migration)
            .unwrap_err()
            .to_string()
            .contains("no catalog identity"));
    }
}

#[test]
fn malformed_later_index_prevents_any_legacy_conversion_write() {
    let (storage, catalog, resolution) = fixture();
    storage.save_catalog_index_row(&row("a_legacy")).unwrap();
    let mut invalid = row("z_invalid");
    invalid.definition_json = Some(
        serde_json::to_string(&IndexDefinition {
            catalog: Some(IndexCatalogIdentity {
                identity: CatalogObjectIdentity {
                    object_id: [3; 16],
                    oid: 17000,
                },
                table_object_id: [9; 16],
                physical_key: "physical".into(),
            }),
            ..IndexDefinition::default()
        })
        .unwrap(),
    );
    storage.save_catalog_index_row(&invalid).unwrap();
    assert!(restore(&storage, &catalog, &resolution, true).is_err());
    assert!(storage
        .load_catalog_indexes()
        .unwrap()
        .iter()
        .find(|row| row.relation.name == "a_legacy")
        .unwrap()
        .definition_json
        .is_none());
    assert!(storage.get_metadata(VERSION).unwrap().is_none());
}

#[test]
fn current_index_addresses_cannot_alias_tables_or_other_index_physical_keys() {
    let (storage, catalog, resolution) = fixture();
    for name in ["a", "b"] {
        storage.save_catalog_index_row(&row(name)).unwrap();
    }
    let valid = restore(&storage, &catalog, &resolution, true).unwrap();
    let first = crate::catalog::index::index_definition(&valid[0])
        .unwrap()
        .catalog
        .unwrap();
    for physical in [false, true] {
        let mut altered = valid[1].clone();
        let mut definition = crate::catalog::index::index_definition(&altered).unwrap();
        let identity = definition.catalog.as_mut().unwrap();
        if physical {
            identity.physical_key.clone_from(&first.physical_key);
        } else {
            identity.identity.oid = uqa_sql::catalog::oids::stable_object_oid("relation", &[1; 16]);
        }
        altered.definition_json = Some(serde_json::to_string(&definition).unwrap());
        storage.save_catalog_index_row(&altered).unwrap();
        assert!(restore(&storage, &catalog, &resolution, true)
            .unwrap_err()
            .to_string()
            .contains("duplicate index"));
        storage.save_catalog_index_row(&valid[1]).unwrap();
    }
}
