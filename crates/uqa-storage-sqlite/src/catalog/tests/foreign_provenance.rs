//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn foreign_table_private_provenance_tracks_exact_names_rename_deletion_and_undo() {
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
        catalog
            .save_foreign_server("remote", "memory_fdw", "{}")
            .unwrap();
        let saved = RelationIdentity::new(schema, "saved:%_\0");
        let neighbor = RelationIdentity::new(schema, "saved:%_\0next");
        let other = RelationIdentity::new("other", &saved.name);
        let renamed = RelationIdentity::new(schema, "renamed");
        let created = RelationIdentity::new(schema, "created");
        let row = |relation: &RelationIdentity| ForeignTableRow {
            relation: relation.clone(),
            security: uqa_storage::RelationSecurityRow::legacy("uqa"),
            server_name: "remote".into(),
            columns_json: "[]".into(),
            options_json: "{}".into(),
        };
        for relation in [&saved, &neighbor, &other] {
            catalog.save_foreign_table(&row(relation)).unwrap();
            assert!(!catalog.foreign_table_has_private_changes(relation).unwrap());
        }
        connection.begin_transaction().unwrap();
        connection.savepoint("foreign_tables").unwrap();
        catalog.save_foreign_table(&row(&saved)).unwrap();
        assert_eq!(
            catalog.foreign_table_has_private_changes(&saved).unwrap(),
            native
        );
        assert!(!catalog.view_has_private_changes(&saved).unwrap());
        catalog.rename_foreign_table(&saved, &renamed).unwrap();
        catalog.save_foreign_table(&row(&created)).unwrap();
        catalog.drop_foreign_table(&renamed).unwrap();
        for relation in [&saved, &renamed, &created] {
            assert_eq!(
                catalog.foreign_table_has_private_changes(relation).unwrap(),
                native
            );
        }
        for relation in [&neighbor, &other, &RelationIdentity::new(schema, "saved")] {
            assert!(!catalog.foreign_table_has_private_changes(relation).unwrap());
        }
        connection.rollback_to_savepoint("foreign_tables").unwrap();
        for relation in [&saved, &renamed, &created] {
            assert!(!catalog.foreign_table_has_private_changes(relation).unwrap());
        }
        assert!(catalog
            .load_foreign_tables()
            .unwrap()
            .iter()
            .any(|row| row.relation == saved));
        catalog.drop_foreign_table(&saved).unwrap();
        connection.commit_transaction().unwrap();
        assert!(!catalog.foreign_table_has_private_changes(&saved).unwrap());
        assert!(!catalog
            .load_foreign_tables()
            .unwrap()
            .iter()
            .any(|row| row.relation == saved));
    }
}
