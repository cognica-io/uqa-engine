//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::catalog::foreign::{reads::*, StoredForeignTable};
use uqa_sql::{ast::RelationPersistence, catalog::roles::RoleIdentity};
use uqa_storage::{KeyValueCatalog, MemoryKeyValueStore};

impl ForeignRegistryReads for RestoredForeignCatalog {
    fn servers(&self) -> ForeignServersRead<'_> {
        Box::new(&self.servers)
    }
    fn tables(&self) -> ForeignTablesRead<'_> {
        Box::new(&self.tables)
    }
    fn security(&self) -> ForeignSecurityRead<'_> {
        Box::new(&self.security)
    }
}

fn table(name: &str, persistence: RelationPersistence) -> StoredForeignTable {
    let mut table = StoredForeignTable::from_catalog(
        name.into(),
        "source".into(),
        BTreeMap::new(),
        r#"{"version":1,"object_id":[2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2],"columns":[]}"#,
    )
    .unwrap()
    .0;
    table.persistence = persistence;
    table
}

fn registry() -> RestoredForeignCatalog {
    RestoredForeignCatalog {
        servers: BTreeMap::new(),
        tables: BTreeMap::new(),
        security: BTreeMap::new(),
    }
}

#[test]
fn temporary_foreign_publication_leaves_durable_definitions_unchanged() {
    let catalog = KeyValueCatalog::new(std::sync::Arc::new(MemoryKeyValueStore::new()));
    catalog
        .save_schema_row(&uqa_storage::SchemaRow::bootstrap("public"))
        .unwrap();
    let security = BoundTableSecurity::owner(RoleIdentity::BOOTSTRAP);
    let permanent = RelationIdentity::new("public", "durable");
    table("public.durable", RelationPersistence::Permanent)
        .persist(Some(&catalog), &permanent, &security)
        .unwrap();
    let before = catalog.load_foreign_tables().unwrap()[0]
        .columns_json
        .clone();
    let temporary = RelationIdentity::new("pg_temp_1", "private");
    let local = table("pg_temp_1.private", RelationPersistence::Temporary);
    local
        .persist(Some(&catalog), &temporary, &security)
        .unwrap();
    let rows = catalog.load_foreign_tables().unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].relation, permanent);
    assert_eq!(rows[0].columns_json, before);
    let restored = StoredForeignTable::from_catalog(
        "public.durable".into(),
        "source".into(),
        BTreeMap::new(),
        &before,
    )
    .unwrap()
    .0;
    assert_eq!(restored.persistence, RelationPersistence::Permanent);
    assert!(!before.contains("persistence"));
}

#[test]
fn durable_refresh_preserves_only_temporary_foreign_definitions_and_security() {
    let mut current = registry();
    let security = BoundTableSecurity::owner(RoleIdentity::BOOTSTRAP);
    let temporary = RelationIdentity::new("pg_temp_1", "private");
    current.tables.insert(
        temporary.clone(),
        table("pg_temp_1.private", RelationPersistence::Temporary),
    );
    current.security.insert(temporary.clone(), security.clone());
    let removed = RelationIdentity::new("public", "removed");
    current.tables.insert(
        removed.clone(),
        table("public.removed", RelationPersistence::Permanent),
    );
    current.security.insert(removed.clone(), security);
    let mut restored = registry();
    restored.retain_temporary(&current).unwrap();
    assert_eq!(restored.tables.len(), 1);
    assert_eq!(
        restored.tables[&temporary].persistence,
        RelationPersistence::Temporary
    );
    assert_eq!(restored.security[&temporary], current.security[&temporary]);
    assert!(!restored.tables.contains_key(&removed));
    assert!(!restored.security.contains_key(&removed));
    let mut latest = table("public.latest", RelationPersistence::Permanent);
    latest.object_id = [9; 16];
    let latest_relation = RelationIdentity::new("public", "latest");
    let merged = crate::catalog::security::relation_authority::merge_private_foreign(
        None,
        &current.tables,
        &current.security,
        BTreeMap::from([(latest_relation.clone(), latest)]).into(),
        BTreeMap::from([(
            latest_relation.clone(),
            BoundTableSecurity::owner(RoleIdentity::BOOTSTRAP),
        )])
        .into(),
    )
    .unwrap();
    assert_eq!(merged.definitions.len(), 2);
    assert!(merged.definitions.contains_key(&latest_relation));
    assert_eq!(merged.security[&temporary], current.security[&temporary]);
    assert!(!merged.definitions.contains_key(&removed));
    current.security.remove(&temporary);
    assert!(restored.retain_temporary(&current).is_err());
}
