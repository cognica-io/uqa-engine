//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn view_private_provenance_tracks_exact_names_rename_deletion_and_undo() {
    for native in [false, true] {
        let connection = ManagedConnection::open_in_memory().unwrap();
        let catalog = Catalog::open(connection.clone()).unwrap();
        if native {
            connection
                .bind_native_records(uqa_storage::mvcc::VersionedSessionOptions::default())
                .unwrap();
        }
        let schema = "tenant:%_\0日本語";
        catalog.save_schema(schema).unwrap();
        catalog.save_schema("other").unwrap();
        let saved = RelationIdentity::new(schema, "saved:%_\0");
        let neighbor = RelationIdentity::new(schema, "saved:%_\0next");
        let other_schema = RelationIdentity::new("other", &saved.name);
        let renamed = RelationIdentity::new(schema, "renamed");
        let created = RelationIdentity::new(schema, "created");
        let row = |relation: &RelationIdentity, definition: &str| ViewRow {
            relation: relation.clone(),
            security: uqa_storage::RelationSecurityRow::legacy("uqa"),
            definition_json: definition.into(),
        };
        for relation in [&saved, &neighbor, &other_schema] {
            catalog.save_view(&row(relation, "{}")).unwrap();
            assert!(!catalog.view_has_private_changes(relation).unwrap());
        }
        connection.begin_transaction().unwrap();
        connection.savepoint("before_views").unwrap();
        catalog
            .save_view(&row(&saved, "{\"replaced\":true}"))
            .unwrap();
        assert_eq!(catalog.view_has_private_changes(&saved).unwrap(), native);
        catalog.rename_view(&saved, &renamed).unwrap();
        catalog.save_view(&row(&created, "{}")).unwrap();
        for relation in [&saved, &renamed, &created] {
            assert_eq!(catalog.view_has_private_changes(relation).unwrap(), native);
        }
        for relation in [
            &neighbor,
            &other_schema,
            &RelationIdentity::new(schema, "saved"),
        ] {
            assert!(!catalog.view_has_private_changes(relation).unwrap());
        }
        catalog.drop_view(&renamed).unwrap();
        assert_eq!(catalog.view_has_private_changes(&renamed).unwrap(), native);
        connection.rollback_to_savepoint("before_views").unwrap();
        for relation in [&saved, &renamed, &created] {
            assert!(!catalog.view_has_private_changes(relation).unwrap());
        }
        assert!(catalog
            .load_views()
            .unwrap()
            .iter()
            .any(|view| view.relation == saved));
        catalog.drop_view(&saved).unwrap();
        connection.commit_transaction().unwrap();
        assert!(!catalog.view_has_private_changes(&saved).unwrap());
        assert!(!catalog
            .load_views()
            .unwrap()
            .iter()
            .any(|view| view.relation == saved));
    }
}
