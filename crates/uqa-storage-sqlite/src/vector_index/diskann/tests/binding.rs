//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_storage::vector_index::DiskANNIndexParams;
use uqa_storage::{
    CatalogIndexRow, RelationIdentity, RelationSecurityRow, TableSchema, VectorFieldSchema,
};

mod identity;
mod live;
mod pruning;
mod publication;
mod selection;
mod validation;

const TABLE: &str = "public.native_binding";
const FIELD: &str = "vector";

fn row() -> CatalogIndexRow {
    CatalogIndexRow {
        relation: RelationIdentity::new("public", "native_binding_idx"),
        index_type: "diskann".into(),
        table_name: TABLE.into(),
        columns_json: "[\"vector\"]".into(),
        parameters_json: serde_json::to_string(
            &DiskANNIndexParams::for_dimensions(2)
                .unwrap()
                .to_catalog_map(2)
                .unwrap(),
        )
        .unwrap(),
        definition_json: None,
    }
}

fn setup(connection: &ManagedConnection) -> Catalog {
    let catalog = Catalog::open(connection.clone()).unwrap();
    catalog.save_schema("public").unwrap();
    catalog
        .save_table(&TableSchema {
            relation: RelationIdentity::new("public", "native_binding"),
            security: RelationSecurityRow::legacy("owner"),
            object_id: [71; 16],
            storage_generation: [72; 16],
            analyzer_json: "{}".into(),
            fts_fields: vec![],
            vector_fields: vec![VectorFieldSchema {
                field: FIELD.into(),
                dimensions: 2,
            }],
            columns_json: "[]".into(),
            constraints_json: "{}".into(),
        })
        .unwrap();
    catalog.save_catalog_index_row(&row()).unwrap();
    catalog
}

fn capture(
    connection: &ManagedConnection,
    control: &StorageReadControl,
) -> RetainedSQLiteDiskANNCanonical {
    canonical(connection, TABLE, FIELD, 2)
        .retain_for_index(&row().relation, control)
        .unwrap()
}

fn guard(
    connection: &ManagedConnection,
    source: &RetainedSQLiteDiskANNCanonical,
    control: &StorageReadControl,
) -> crate::Result<()> {
    connection
        .with_native_write(|snapshot, batch| {
            source.require_current_index(&snapshot.record_read(), batch, control)?;
            snapshot.put_row(
                batch,
                Family::Metadata,
                crate::mvcc::native::NativeRecordOwner::Database(snapshot.database),
                &[
                    ValueRef::Text(b"diskann-bound-write"),
                    ValueRef::Text(b"accepted"),
                ],
            )?;
            Ok(())
        })?
        .expect("bound native fixture");
    Ok(())
}

#[test]
fn native_diskann_catalog_binding_retains_real_definitions_in_all_file_modes() {
    for mode in 0..4 {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("binding.db");
        let connection = open(&path, mode);
        let catalog = setup(&connection);
        let control = StorageReadControl::with_limit(1 << 20);
        let version = canonical(&connection, TABLE, FIELD, 2)
            .replace(1, &[vec![1.0, 0.0]], &control)
            .unwrap();
        let source = capture(&connection, &control);
        assert_eq!(
            source.index_parameters(),
            Some(DiskANNIndexParams::for_dimensions(2).unwrap())
        );
        let foreign = open(&directory.path().join("foreign.db"), mode);
        let foreign_catalog = setup(&foreign);
        assert!(guard(&foreign, &source, &control).is_err());
        assert_eq!(
            foreign_catalog.get_metadata("diskann-bound-write").unwrap(),
            None
        );
        assert!(guard(
            &connection,
            &canonical(&connection, TABLE, FIELD, 2)
                .retain(&control)
                .unwrap(),
            &control
        )
        .is_err());
        canonical(&connection, TABLE, FIELD, 2)
            .replace(1, &[], &control)
            .unwrap();
        guard(&connection, &source, &control).unwrap();
        drop((catalog, connection));
        let connection = open(&path, mode);
        assert_eq!(source.origin(1, &control).unwrap(), Some(version));
        guard(&connection, &source, &control).unwrap();
        private_undo(&connection, &control);
        definition_race(&path, mode, &connection, &control);
    }
}

fn private_undo(connection: &ManagedConnection, control: &StorageReadControl) {
    let catalog = Catalog::open(connection.clone()).unwrap();
    connection.begin_transaction().unwrap();
    connection.savepoint("native_binding").unwrap();
    catalog.save_catalog_index_row(&row()).unwrap();
    let source = capture(connection, control);
    canonical(connection, TABLE, FIELD, 2)
        .replace(2, &[], control)
        .unwrap();
    guard(connection, &source, control).unwrap();
    assert!(connection.in_transaction());
    connection.rollback_to_savepoint("native_binding").unwrap();
    assert!(guard(connection, &source, control).is_err());
    catalog.save_catalog_index_row(&row()).unwrap();
    assert!(guard(connection, &source, control).is_err());
    connection.rollback_transaction().unwrap();
}

fn definition_race(
    path: &Path,
    mode: u8,
    connection: &ManagedConnection,
    control: &StorageReadControl,
) {
    let catalog = Catalog::open(connection.clone()).unwrap();
    catalog.delete_metadata("diskann-bound-write").unwrap();
    let peer = open(path, mode);
    let peer_catalog = Catalog::open(peer.clone()).unwrap();
    let source = capture(connection, control);
    connection.begin_transaction().unwrap();
    guard(connection, &source, control).unwrap();
    peer_catalog.save_catalog_index_row(&row()).unwrap();
    assert!(connection.commit_transaction().is_err());
    connection.rollback_transaction().unwrap();
    assert_eq!(catalog.get_metadata("diskann-bound-write").unwrap(), None);
    let source = capture(connection, control);
    connection.begin_transaction().unwrap();
    guard(connection, &source, control).unwrap();
    canonical(&peer, TABLE, FIELD, 2)
        .replace(99, &[], control)
        .unwrap();
    connection.commit_transaction().unwrap();
    assert_eq!(
        catalog
            .get_metadata("diskann-bound-write")
            .unwrap()
            .as_deref(),
        Some("accepted")
    );
}

#[test]
fn native_diskann_catalog_binding_rejects_recreated_index_table_and_storage_owner() {
    let connection = memory();
    let catalog = setup(&connection);
    let control = StorageReadControl::with_limit(1 << 20);
    let source = capture(&connection, &control);
    catalog.drop_catalog_index(&row().relation).unwrap();
    assert!(guard(&connection, &source, &control).is_err());
    catalog.save_catalog_index_row(&row()).unwrap();
    assert!(guard(&connection, &source, &control).is_err());
    let source = capture(&connection, &control);
    let mut schema = catalog
        .load_tables()
        .unwrap()
        .into_iter()
        .find(|schema| schema.relation.qualified_name() == TABLE)
        .unwrap();
    schema.storage_generation = [73; 16];
    catalog.save_table(&schema).unwrap();
    assert!(guard(&connection, &source, &control).is_err());
    let source = capture(&connection, &control);
    catalog.drop_table_and_data(TABLE).unwrap();
    setup(&connection);
    assert!(guard(&connection, &source, &control).is_err());
}
