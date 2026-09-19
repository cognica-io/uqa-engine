//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::catalog::relation_acl::{key, prefix, RelationAclTuple};
use uqa_core::catalog_role::{BoundAclEntry, RoleIdentity};

fn tuple() -> RelationAclTuple {
    RelationAclTuple::new(Some(vec![BoundAclEntry {
        role: None,
        grantor: RoleIdentity::BOOTSTRAP,
        privileges: crate::TablePrivileges::ALL,
        grant_options: crate::TablePrivileges::default(),
    }]))
    .unwrap()
}

fn create_relation(catalog: &KeyValueCatalog, kind: RelationKind, relation: &RelationIdentity) {
    match kind {
        RelationKind::Table => catalog
            .save_table(&TableSchema {
                relation: relation.clone(),
                security: RelationSecurityRow::bootstrap(),
                object_id: [1; 16],
                storage_generation: [2; 16],
                analyzer_json: "{}".into(),
                fts_fields: vec![],
                vector_fields: vec![],
                columns_json: "[]".into(),
                constraints_json: String::new(),
            })
            .unwrap(),
        RelationKind::View => catalog
            .save_view(&ViewRow {
                relation: relation.clone(),
                security: RelationSecurityRow::bootstrap(),
                definition_json: "{}".into(),
            })
            .unwrap(),
        RelationKind::ForeignTable => catalog
            .save_foreign_table(&ForeignTableRow {
                relation: relation.clone(),
                security: RelationSecurityRow::bootstrap(),
                server_name: "remote".into(),
                columns_json: "[]".into(),
                options_json: "{}".into(),
            })
            .unwrap(),
        _ => unreachable!(),
    }
}

fn load_security(
    catalog: &KeyValueCatalog,
    kind: RelationKind,
    name: &RelationIdentity,
) -> RelationSecurityRow {
    match kind {
        RelationKind::Table => {
            catalog
                .load_tables()
                .unwrap()
                .into_iter()
                .find(|row| &row.relation == name)
                .unwrap()
                .security
        }
        RelationKind::View => {
            catalog
                .load_views()
                .unwrap()
                .into_iter()
                .find(|row| &row.relation == name)
                .unwrap()
                .security
        }
        RelationKind::ForeignTable => {
            catalog
                .load_foreign_tables()
                .unwrap()
                .into_iter()
                .find(|row| &row.relation == name)
                .unwrap()
                .security
        }
        _ => unreachable!(),
    }
}

#[test]
fn independent_acl_records_follow_renames_and_compact_with_definitions() {
    for kind in [
        RelationKind::Table,
        RelationKind::View,
        RelationKind::ForeignTable,
    ] {
        let catalog = KeyValueCatalog::new(Arc::new(MemoryKeyValueStore::new()));
        catalog.save_schema("public").unwrap();
        let relation = RelationIdentity::new("public", "original");
        let renamed = RelationIdentity::new("public", "renamed");
        create_relation(&catalog, kind, &relation);
        let first = tuple();
        let second = tuple();
        // Exercise the versioned layout directly; the memory facade's public write path uses its serialized definition format.
        for (column, entry) in [(None, &first), (Some("a:%\0"), &second)] {
            catalog
                .set_metadata(
                    &key(&relation, column),
                    &serde_json::to_string(entry).unwrap(),
                )
                .unwrap();
        }
        let load = |name: &RelationIdentity| load_security(&catalog, kind, name);
        let expected = load(&relation);
        match kind {
            RelationKind::Table => catalog
                .rename_table_data("public.original", "public.renamed")
                .unwrap(),
            RelationKind::View => assert!(catalog.rename_view(&relation, &renamed).unwrap()),
            RelationKind::ForeignTable => {
                assert!(catalog.rename_foreign_table(&relation, &renamed).unwrap());
            }
            _ => unreachable!(),
        }
        assert_eq!(load(&renamed), expected);
        assert!(catalog
            .metadata_with_prefix(&prefix(&relation))
            .unwrap()
            .is_empty());
        match kind {
            RelationKind::Table => {
                let row = catalog.load_tables().unwrap().remove(0);
                catalog.save_table(&row).unwrap();
            }
            RelationKind::View => {
                let row = catalog.load_views().unwrap().remove(0);
                catalog.save_view(&row).unwrap();
            }
            RelationKind::ForeignTable => {
                let row = catalog.load_foreign_tables().unwrap().remove(0);
                catalog.save_foreign_table(&row).unwrap();
            }
            _ => unreachable!(),
        }
        assert!(catalog
            .metadata_with_prefix(&prefix(&renamed))
            .unwrap()
            .is_empty());
        let RelationSecurityRow::Bound(restored) = load(&renamed) else {
            panic!("missing bound security");
        };
        assert_eq!(restored.acl, first.acl);
        assert_eq!(restored.column_acls["a:%\0"], second.acl.clone().unwrap());
        assert!(restored.acl_revisions.is_empty());
        catalog
            .set_metadata(
                &key(&renamed, None),
                &serde_json::to_string(&tuple()).unwrap(),
            )
            .unwrap();
        match kind {
            RelationKind::Table => catalog.drop_table_and_data("public.renamed").unwrap(),
            RelationKind::View => assert!(catalog.drop_view(&renamed).unwrap()),
            RelationKind::ForeignTable => catalog.drop_foreign_table(&renamed).unwrap(),
            _ => unreachable!(),
        }
        assert!(catalog
            .metadata_with_prefix(&prefix(&renamed))
            .unwrap()
            .is_empty());
    }
}

#[test]
fn serialized_catalogs_keep_acl_changes_in_their_definition_format() {
    let catalog = KeyValueCatalog::new(Arc::new(MemoryKeyValueStore::new()));
    catalog.save_schema("public").unwrap();
    let relation = RelationIdentity::new("public", "visible");
    catalog
        .save_view(&ViewRow {
            relation: relation.clone(),
            security: RelationSecurityRow::bootstrap(),
            definition_json: "{}".into(),
        })
        .unwrap();
    let entry = tuple();
    catalog
        .save_relation_acl(&relation, Some("a"), &entry)
        .unwrap();
    let RelationSecurityRow::Bound(security) = catalog.load_views().unwrap().remove(0).security
    else {
        panic!("missing security");
    };
    assert_eq!(security.column_acls["a"], entry.acl.unwrap());
    assert!(catalog
        .metadata_with_prefix(&prefix(&relation))
        .unwrap()
        .is_empty());
}
