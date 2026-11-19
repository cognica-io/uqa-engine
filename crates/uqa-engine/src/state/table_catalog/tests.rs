//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use crate::{tests::providers::Provider, Engine};
use uqa_core::{
    image_io::{ImageDecodeWindow, ImageDecoder},
    memory::{MemoryBudget, ProductionControl},
    CancellationToken, RelationIdentity,
};
use uqa_execution::catalog::CatalogReadSnapshot;
use uqa_sql::SQLError;

#[rstest::rstest]
#[case::memory(Provider::Memory)]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn catalog_table_capture_keeps_selected_metadata_through_suspended_names_and_source_retirement(
    #[case] provider: Provider,
) {
    let directory = tempfile::tempdir().unwrap();
    let engine = provider.open(&directory.path().join("catalog"));
    engine
        .sql("CREATE TABLE items(id integer PRIMARY KEY, body text)", &[])
        .unwrap();
    let table = engine.try_table("items").unwrap().unwrap();
    let mut snapshot = None;
    let selection = allocation_counter::measure(|| snapshot = Some(table.catalog_snapshot()));
    assert_eq!(selection.bytes_total, 0);
    let snapshot = snapshot.unwrap();
    assert!(snapshot
        .row_type_array_name
        .shares_definition(&table.row_type_array_name.snapshot()));
    let source = engine.storage.tables.live_snapshot();
    let native = MemoryBudget::new(1 << 20);
    let work = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let original = token.clone();
    let retained_native = native.clone();
    let mut input = ImageDecoder::<_, SQLError>::new(&work, &token, |mut reader| async move {
        let mut input = reader.input();
        CatalogReadSnapshot::capture_tables_from_input(
            source.iter().map(crate::TableState::catalog_snapshot_entry),
            &retained_native,
            &original,
            &mut input,
        )
        .await
    })
    .unwrap();
    assert!(input
        .advance(
            &mut std::io::empty(),
            ImageDecodeWindow { bytes: 1, steps: 1 }
        )
        .unwrap()
        .is_none());
    engine.sql("ALTER TABLE items ADD COLUMN later text; CREATE TYPE _items AS ENUM ('occupied'); CREATE TABLE later_table(id integer)", &[]).unwrap();
    let current = engine
        .catalog_read_view_with_control(&ProductionControl::uncontrolled())
        .unwrap();
    let identity = RelationIdentity::new("public", "items");
    assert_eq!(
        current.snapshot().tables[&identity]
            .row_type_array_name
            .as_deref(),
        Some("__items")
    );
    drop((table, current, engine));
    let retained = input.read(&mut std::io::empty()).unwrap();
    assert_eq!(retained.len(), 1);
    let selected = &retained[&identity];
    assert_eq!(selected.columns.len(), 2);
    assert_eq!(selected.row_type_array_name.as_deref(), Some("_items"));
    assert!(selected
        .row_type_array_name
        .shares_definition(&snapshot.row_type_array_name));
    assert!(selected.columns.shares_definition(&snapshot.columns));
    assert_eq!(work.used(), 0);
    assert!(native.used() > 0);
    drop((retained, snapshot));
    assert_eq!(native.used(), 0);
}

#[test]
fn inline_table_catalog_capture_keeps_original_array_name_owners() {
    let engine = Engine::new();
    engine.sql("CREATE TABLE items(id integer)", &[]).unwrap();
    let table = engine.try_table("items").unwrap().unwrap();
    let original = table.row_type_array_name.snapshot();
    let native = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let view = engine
        .catalog_read_view_with_control(&ProductionControl::new(&native, &token, &token))
        .unwrap();
    let identity = RelationIdentity::new("public", "items");
    assert!(view.snapshot().tables[&identity]
        .row_type_array_name
        .shares_definition(&original));
    engine
        .sql("CREATE TYPE _items AS ENUM ('occupied')", &[])
        .unwrap();
    assert_eq!(
        view.snapshot().tables[&identity]
            .row_type_array_name
            .as_deref(),
        Some("_items")
    );
    drop((table, engine, view));
    assert_eq!(native.used(), 0);
    assert_eq!(original.as_deref(), Some("_items"));
}
