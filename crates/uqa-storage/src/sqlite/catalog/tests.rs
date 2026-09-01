//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

mod migration;
mod version_migrations;

fn fresh() -> Catalog {
    let mc = ManagedConnection::open_in_memory().unwrap();
    Catalog::open(mc).unwrap()
}

#[test]
fn migration_creates_tables_table() {
    let cat = fresh();
    cat.conn
        .with(|c| {
            let count: u32 = c.query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE name = '_tables'",
                [],
                |r| r.get(0),
            )?;
            assert_eq!(count, 1);
            Ok(())
        })
        .unwrap();
}

#[test]
fn save_load_round_trip() {
    let cat = fresh();
    let schema = TableSchema {
        relation: RelationIdentity::new("public", "articles"),
        object_id: [1; 16],
        storage_generation: [1; 16],
        analyzer_json:
            "{\"tokenizer\":{\"type\":\"standard\"},\"token_filters\":[],\"char_filters\":[]}"
                .into(),
        fts_fields: vec!["title".into(), "body".into()],
        vector_fields: vec![VectorFieldSchema {
            field: "embedding".into(),
            dimensions: 768,
        }],
        columns_json: String::new(),
        constraints_json: r#"{"checks":[],"foreign_keys":[],"key_constraints":[]}"#.into(),
    };
    cat.save_table(&schema).unwrap();
    let loaded = cat.load_tables().unwrap();
    assert_eq!(loaded.len(), 1);
    assert_eq!(loaded[0].relation.qualified_name(), "public.articles");
    assert_eq!(loaded[0].object_id, [1; 16]);
    assert_eq!(loaded[0].storage_generation, [1; 16]);
    assert_eq!(loaded[0].fts_fields, vec!["title", "body"]);
    assert_eq!(loaded[0].vector_fields.len(), 1);
    assert_eq!(loaded[0].vector_fields[0].field, "embedding");
    assert_eq!(loaded[0].vector_fields[0].dimensions, 768);
    assert!(loaded[0].columns_json.is_empty());
    assert_eq!(loaded[0].constraints_json, schema.constraints_json);
}

#[test]
fn catalog_facade_trait_object_round_trips_table() {
    let cat = fresh();
    let facade: &dyn CatalogFacade = &cat;
    let schema = TableSchema {
        relation: RelationIdentity::new("public", "facade_articles"),
        object_id: [2; 16],
        storage_generation: [2; 16],
        analyzer_json:
            "{\"tokenizer\":{\"type\":\"standard\"},\"token_filters\":[],\"char_filters\":[]}"
                .into(),
        fts_fields: vec!["title".into()],
        vector_fields: vec![VectorFieldSchema {
            field: "embedding".into(),
            dimensions: 128,
        }],
        columns_json: String::new(),
        constraints_json: String::new(),
    };
    facade.save_table(&schema).unwrap();
    let loaded = facade.load_tables().unwrap();
    assert_eq!(loaded.len(), 1);
    assert_eq!(
        loaded[0].relation.qualified_name(),
        "public.facade_articles"
    );
}

#[test]
fn sequence_set_value_preserves_the_next_allocation_state() {
    let connection = ManagedConnection::open_in_memory().unwrap();
    let catalog = Catalog::open(connection).unwrap();
    let object_id = [7; 16];
    let owner = crate::catalog::SequenceOwner {
        table_object_id: [5; 16],
        column_object_id: [6; 16],
        dependency: crate::catalog::SequenceOwnerDependency::Internal,
    };
    catalog
        .create_sequence_row(&SequenceRow {
            relation: RelationIdentity::new("public", "controlled"),
            role_owner: "sequence_owner".into(),
            object_id,
            definition_generation: object_id,
            start: 1,
            increment: 2,
            current: 1,
            called: false,
            persistence: "p".into(),
            options: SequenceOptions::default(),
            owner: Some(owner),
        })
        .unwrap();

    assert_eq!(
        catalog
            .next_sequence_value("public.controlled", [8; 16])
            .unwrap(),
        None
    );
    assert_eq!(
        catalog
            .set_sequence_value("public.controlled", object_id, 7, false)
            .unwrap(),
        Some(7)
    );
    let uncalled = catalog.load_sequence_rows().unwrap().remove(0);
    assert_eq!(uncalled.current, 7);
    assert!(!uncalled.called);
    assert_eq!(uncalled.owner, Some(owner));
    assert_eq!(uncalled.role_owner, "sequence_owner");
    assert_eq!(
        catalog
            .next_sequence_value("public.controlled", object_id)
            .unwrap(),
        Some(7)
    );
    assert_eq!(
        catalog
            .next_sequence_value("public.controlled", object_id)
            .unwrap(),
        Some(9)
    );
    assert_eq!(
        catalog
            .set_sequence_value("public.controlled", object_id, 20, true)
            .unwrap(),
        Some(20)
    );
    assert_eq!(
        catalog
            .next_sequence_value("public.controlled", object_id)
            .unwrap(),
        Some(22)
    );

    let cycling_id = [9; 16];
    catalog
        .create_sequence_row(&SequenceRow {
            relation: RelationIdentity::new("public", "cycling"),
            role_owner: "uqa".into(),
            object_id: cycling_id,
            definition_generation: cycling_id,
            start: 5,
            increment: 3,
            current: 5,
            called: false,
            persistence: "p".into(),
            options: crate::catalog::SequenceOptions {
                data_type: "integer".into(),
                min_value: Some(2),
                max_value: Some(5),
                cycle: true,
                cache_size: 1,
            },
            owner: None,
        })
        .unwrap();
    for expected in [5, 2, 5, 2] {
        assert_eq!(
            catalog
                .next_sequence_value("public.cycling", cycling_id)
                .unwrap(),
            Some(expected)
        );
    }
}

#[test]
fn sqlite_sequence_rename_moves_catalog_identity_and_value_atomically() {
    let catalog = fresh();
    let object_id = [17; 16];
    catalog.save_schema("archive").unwrap();
    catalog
        .create_sequence_row(&SequenceRow {
            relation: RelationIdentity::new("public", "ids"),
            role_owner: "uqa".into(),
            object_id,
            definition_generation: [18; 16],
            start: 1,
            increment: 1,
            current: 7,
            called: true,
            persistence: "u".into(),
            options: SequenceOptions {
                cache_size: 3,
                ..SequenceOptions::default()
            },
            owner: None,
        })
        .unwrap();

    assert!(catalog
        .rename_sequence_row("public.ids", "archive.renamed_ids")
        .unwrap());
    assert_eq!(
        catalog
            .next_sequence_value("public.ids", object_id)
            .unwrap(),
        None
    );
    assert_eq!(
        catalog
            .next_sequence_value("archive.renamed_ids", object_id)
            .unwrap(),
        Some(8)
    );
    let row = catalog.load_sequence_rows().unwrap().remove(0);
    assert_eq!(
        row.relation,
        RelationIdentity::new("archive", "renamed_ids")
    );
    assert_eq!(row.object_id, object_id);
    assert_eq!(row.definition_generation, [18; 16]);
    assert_eq!(row.persistence, "u");

    catalog
        .create_sequence_row(&SequenceRow {
            relation: RelationIdentity::new("public", "occupied"),
            role_owner: "uqa".into(),
            object_id: [19; 16],
            definition_generation: [19; 16],
            start: 1,
            increment: 1,
            current: 1,
            called: false,
            persistence: "p".into(),
            options: SequenceOptions::default(),
            owner: None,
        })
        .unwrap();
    assert!(catalog
        .rename_sequence_row("archive.renamed_ids", "public.occupied")
        .unwrap_err()
        .to_string()
        .contains("already exists"));
    assert_eq!(catalog.load_sequence_rows().unwrap().len(), 2);
}

#[test]
fn sqlite_sequence_reservations_are_atomic_and_stop_at_the_configured_bound() {
    let connection = ManagedConnection::open_in_memory().unwrap();
    let catalog = Catalog::open(connection).unwrap();
    let object_id = [11; 16];
    let generation = [12; 16];
    catalog
        .create_sequence_row(&SequenceRow {
            relation: RelationIdentity::new("public", "cached"),
            role_owner: "uqa".into(),
            object_id,
            definition_generation: generation,
            start: 1,
            increment: 1,
            current: 1,
            called: false,
            persistence: "p".into(),
            options: SequenceOptions {
                min_value: Some(1),
                max_value: Some(3),
                cache_size: 5,
                ..SequenceOptions::default()
            },
            owner: None,
        })
        .unwrap();

    assert_eq!(
        catalog
            .reserve_sequence_values("public.cached", object_id, generation)
            .unwrap(),
        SequenceReservationResult::Reserved(crate::catalog::SequenceValueReservation {
            first_value: 1,
            last_value: 3,
            count: 3,
        })
    );
    assert_eq!(
        catalog
            .reserve_sequence_values("public.cached", object_id, generation)
            .unwrap(),
        SequenceReservationResult::Exhausted
    );
    let mut row = catalog.load_sequence_rows().unwrap().remove(0);
    row.options.cycle = true;
    row.definition_generation = [13; 16];
    assert!(catalog.replace_sequence_row(&row).unwrap());
    assert_eq!(
        catalog
            .reserve_sequence_values("public.cached", object_id, generation)
            .unwrap(),
        SequenceReservationResult::DefinitionChanged
    );
    assert_eq!(
        catalog
            .reserve_sequence_values("public.cached", object_id, [13; 16])
            .unwrap(),
        SequenceReservationResult::Reserved(crate::catalog::SequenceValueReservation {
            first_value: 1,
            last_value: 3,
            count: 3,
        })
    );
}

#[test]
fn corrupt_catalog_index_columns_abort_column_lifecycle() {
    let cat = fresh();
    cat.save_catalog_index("broken", "btree", "docs", "not-json", "{}")
        .unwrap();

    assert!(matches!(
        cat.drop_column_data("docs", "title"),
        Err(SQLiteError::Serde(_))
    ));
    assert_eq!(cat.load_catalog_indexes().unwrap().len(), 1);
    assert!(matches!(
        cat.rename_column_data("docs", "title", "headline"),
        Err(SQLiteError::Serde(_))
    ));
    assert_eq!(
        cat.load_catalog_indexes().unwrap()[0].columns_json,
        "not-json"
    );
}

#[test]
fn negative_graph_ids_are_reported_as_catalog_corruption() {
    let cat = fresh();
    cat.conn
        .with(|connection| {
            connection.execute(
                "INSERT INTO _graph_vertices (vertex_id, label, properties_json)
                 VALUES (-1, 'person', '{}')",
                [],
            )?;
            Ok(())
        })
        .unwrap();
    assert!(matches!(
        cat.load_vertices(),
        Err(SQLiteError::StorageBackend(message))
            if message.contains("negative vertex id -1")
    ));

    cat.conn
        .with(|connection| {
            connection.execute("DELETE FROM _graph_vertices", [])?;
            connection.execute(
                "INSERT INTO _graph_edges
                    (edge_id, source_id, target_id, label, properties_json)
                 VALUES (1, -2, 3, 'knows', '{}')",
                [],
            )?;
            Ok(())
        })
        .unwrap();
    assert!(matches!(
        cat.load_edges(),
        Err(SQLiteError::StorageBackend(message))
            if message.contains("negative edge source vertex id -2")
    ));

    cat.conn
        .with(|connection| {
            connection.execute(
                "INSERT INTO _graph_membership (entity_type, entity_id, graph_name)
                 VALUES ('vertex', -3, 'g')",
                [],
            )?;
            Ok(())
        })
        .unwrap();
    assert!(matches!(
        cat.load_graph_memberships(),
        Err(SQLiteError::StorageBackend(message))
            if message.contains("negative graph membership entity id -3")
    ));
}

#[test]
fn graph_ids_beyond_sqlite_integer_range_are_rejected_before_write() {
    let cat = fresh();

    assert!(matches!(
        cat.save_vertex(u64::MAX, "person", "{}"),
        Err(SQLiteError::StorageBackend(message))
            if message.contains("exceeds the SQLite INTEGER range")
    ));
    assert!(cat.load_vertices().unwrap().is_empty());
}
