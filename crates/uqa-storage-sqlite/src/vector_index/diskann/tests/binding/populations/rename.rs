//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::mvcc::native::{decode_record, NativeRecord, NativeRecordFamily as Family};
use crate::{Catalog, SQLiteVectorIndex};
use rusqlite::types::ValueRef;
use uqa_storage::key_value::{conformance::build_diskann_publication_fixture, KeyValueRead};

const DESTINATION: &str = "occupied_vector";

pub(super) fn image(connection: &ManagedConnection) -> Vec<(Vec<u8>, Vec<u8>)> {
    let snapshot = connection.native_snapshot().unwrap().unwrap();
    let mut rows = Vec::new();
    snapshot
        .record_read()
        .visit_prefix(b"", &mut |key, value| {
            rows.push((key.to_vec(), value.to_vec()));
            Ok(())
        })
        .unwrap();
    rows
}

#[test]
fn native_diskann_population_rename_rejects_independent_generations_and_partial_destinations() {
    for mode in 0..4 {
        let directory = tempfile::tempdir().unwrap();
        let connection = open(&directory.path().join("population-rename.db"), mode);
        let control = StorageReadControl::with_limit(1 << 22);
        seed(&connection, &control);
        let catalog = Catalog::open(connection.clone()).unwrap();
        raw_and_catalog(&connection, &catalog);
        connection.begin_transaction().unwrap();
        let generation = second(&connection, &catalog, &control);
        let before = image(&connection);
        assert!(catalog
            .rename_column_data(TABLE, FIELD, DESTINATION)
            .is_err());
        assert_eq!(image(&connection), before);
        let destination = canonical(&connection, TABLE, DESTINATION, 2);
        assert_eq!(
            destination
                .retain(&control)
                .unwrap()
                .population_counts(generation, &control)
                .unwrap(),
            Some(DiskANNCanonicalCounts::new(1, 0).unwrap())
        );
        catalog.rename_column_data(TABLE, FIELD, FIELD).unwrap();
        assert_eq!(image(&connection), before);
        connection.rollback_transaction().unwrap();
        partial(&connection, &catalog, &control);
        counts(&live(&connection, &control), 3, 0);
    }
}

fn second(
    connection: &ManagedConnection,
    catalog: &Catalog,
    control: &StorageReadControl,
) -> DiskANNGeneration {
    let mut table = catalog
        .load_tables()
        .unwrap()
        .into_iter()
        .find(|table| table.relation.qualified_name() == TABLE)
        .unwrap();
    table.vector_fields.push(uqa_storage::VectorFieldSchema {
        field: DESTINATION.into(),
        dimensions: 2,
    });
    catalog.save_table(&table).unwrap();
    let mut definition = row();
    definition.relation.name.push_str("_second");
    definition.definition_json = Some(serde_json::to_string(&[93; 16]).unwrap());
    definition.columns_json = serde_json::to_string(&[DESTINATION]).unwrap();
    catalog.save_catalog_index_row(&definition).unwrap();
    let owner = canonical(connection, TABLE, DESTINATION, 2);
    owner.replace(9, &[vec![0.0, 1.0]], control).unwrap();
    owner.replace(10, &[], control).unwrap();
    let source = owner
        .retain_for_index(&definition.relation, control)
        .unwrap();
    let parameters = source.index_parameters().unwrap();
    let scope = source.index_scope(&Resolver, control).unwrap();
    let repository = connection.diskann_generations(control).unwrap();
    let mut stage = repository.allocate_bound_stage(&scope, control).unwrap();
    let coverage =
        build_diskann_publication_fixture(source, &mut stage, parameters, control).unwrap();
    connection
        .publish_diskann_generation(&coverage, &Resolver, control)
        .unwrap();
    stage.generation()
}

fn partial(connection: &ManagedConnection, catalog: &Catalog, control: &StorageReadControl) {
    let source = image(connection);
    for family in [
        Family::Vectors,
        Family::VectorOrigins,
        Family::VectorChanges,
        Family::VectorPopulations,
        Family::VectorPopulationWitnesses,
    ] {
        let (key, value) = source
            .iter()
            .find(|(key, value)| {
                let (identity, row) = decode_record(key, value, control).unwrap();
                identity.family() == family && row[1] == ValueRef::Text(FIELD.as_bytes())
            })
            .unwrap();
        let (identity, mut row) = decode_record(key, value, control).unwrap();
        row[1] = ValueRef::Text(DESTINATION.as_bytes());
        let record = NativeRecord::encode(family, identity.owner(), &row, control).unwrap();
        connection.begin_transaction().unwrap();
        connection
            .with_native_write(|_, batch| {
                batch.put(record.key(), record.row())?;
                Ok(())
            })
            .unwrap()
            .unwrap();
        let before = image(connection);
        assert!(catalog
            .rename_column_data(TABLE, FIELD, DESTINATION)
            .is_err());
        assert_eq!(image(connection), before);
        connection.rollback_transaction().unwrap();
    }
}

fn raw_and_catalog(connection: &ManagedConnection, catalog: &Catalog) {
    connection.begin_transaction().unwrap();
    let mut source = SQLiteVectorIndex::new(connection.clone(), TABLE, "raw_before", 2);
    let mut target = SQLiteVectorIndex::new(connection.clone(), TABLE, "raw_after", 2);
    source.add(1, vec![1.0, 0.0]).unwrap();
    source.add(3, vec![1.0, 0.0]).unwrap();
    target.add(1, vec![0.0, 1.0]).unwrap();
    target.add(2, vec![-1.0, 0.0]).unwrap();
    let control = StorageReadControl::with_limit(1 << 22);
    let preserved: Vec<_> = image(connection)
        .into_iter()
        .filter(|(key, value)| {
            let (identity, row) = decode_record(key, value, &control).unwrap();
            identity.family() == Family::Vectors && row[1] == ValueRef::Text(b"raw_after")
        })
        .collect();
    catalog
        .rename_column_data(TABLE, "raw_before", "raw_after")
        .unwrap();
    assert_eq!(source.count().unwrap(), 0);
    assert_eq!(target.count().unwrap(), 3);
    let renamed = image(connection);
    for row in preserved {
        assert!(renamed.contains(&row));
    }
    connection.rollback_transaction().unwrap();
    for field in ["legacy_before", "legacy_after"] {
        connection.begin_transaction().unwrap();
        let mut target = SQLiteVectorIndex::new(connection.clone(), TABLE, "legacy_after", 2);
        target.add(1, vec![1.0, 0.0]).unwrap();
        let mut definition = row();
        definition.relation.name.push_str("_legacy");
        definition.columns_json = serde_json::to_string(&[field]).unwrap();
        catalog.save_catalog_index_row(&definition).unwrap();
        let before = image(connection);
        let error = catalog
            .rename_column_data(TABLE, "legacy_before", "legacy_after")
            .unwrap_err();
        assert!(error
            .to_string()
            .contains("column rename would merge DiskANN canonical fields"));
        assert_eq!(image(connection), before);
        connection.rollback_transaction().unwrap();
    }
}
