//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{state::SnapshotCut, Engine};
use std::sync::Arc;

fn encoded(source: &SnapshotCut, slot: u16, control: &StorageReadControl) -> Vec<u8> {
    let mut reader = crate::backup::memory::catalogs::image(source, slot, control)
        .unwrap()
        .unwrap();
    let mut bytes = Vec::new();
    let mut buffer = [0; 4096];
    loop {
        let count = reader.read(&mut buffer).unwrap();
        if count == 0 {
            break;
        }
        bytes.extend_from_slice(&buffer[..count]);
    }
    bytes
}

#[test]
fn ordered_catalog_input_preserves_all_native_images_without_sql_replay() {
    let source = Engine::new();
    for sql in [
        "CREATE TYPE mood AS ENUM ('low', 'high')",
        "CREATE DOMAIN positive AS integer CHECK (VALUE > 0)",
        "CREATE SEQUENCE seq START 11 INCREMENT 3",
        "SELECT nextval('seq')",
        "CREATE VIEW constant_value AS SELECT 7 AS value",
        "CREATE FUNCTION plus_one(x integer) RETURNS integer LANGUAGE SQL AS 'SELECT x + 1'",
        "CREATE ROLE reader",
        "CREATE SCHEMA app AUTHORIZATION reader",
        "COMMENT ON TYPE mood IS 'original metadata'",
    ] {
        source
            .sql(sql, &[])
            .unwrap_or_else(|error| panic!("{sql}: {error}"));
    }
    let source_control = StorageReadControl::with_limit(16 << 20);
    let selected = source.capture_committed_memory(&source_control).unwrap();
    drop(source);
    let mut destination = Engine::new();
    let registry = destination.storage.memory_capture.as_ref().unwrap();
    let control = StorageReadControl::with_limit(16 << 20);
    let mutations = MemoryBudget::new(16 << 20);
    let mut input = CatalogInput::new();
    let mut expected = Vec::new();
    for slot in 1..=uqa_storage::backup::MEMORY_IMAGE_CATALOG_COUNT {
        let bytes = encoded(&selected, slot, &source_control);
        input = input
            .read(
                Arc::get_mut(&mut destination.durable).unwrap(),
                slot,
                &mut bytes.as_slice(),
                &Destination {
                    registry,
                    mutations: &mutations,
                    control: &control,
                },
            )
            .unwrap();
        expected.push(bytes);
    }
    input.finish().unwrap();
    drop(selected);
    assert_eq!(source_control.memory().used(), 0);
    control.cancellation().cancel();
    let full = control
        .memory()
        .reserve(control.memory().available())
        .unwrap();
    let capture_control = StorageReadControl::with_limit(16 << 20);
    let selected = destination
        .capture_committed_memory(&capture_control)
        .unwrap();
    drop((destination, full));
    for (offset, bytes) in expected.iter().enumerate() {
        assert_eq!(
            &encoded(
                &selected,
                u16::try_from(offset + 1).unwrap(),
                &capture_control
            ),
            bytes
        );
    }
    assert!(control.memory().used() > 0);
    drop(selected);
    assert_eq!(control.memory().used(), 0);
    assert_eq!(capture_control.memory().used(), 0);
}

#[test]
fn catalog_input_rejects_missing_and_out_of_order_components_before_completion() {
    assert!(CatalogInput::new().finish().is_err());
    let mut destination = Engine::new();
    let control = StorageReadControl::with_limit(65536);
    let mutations = MemoryBudget::new(65536);
    assert!(CatalogInput::new()
        .read(
            Arc::get_mut(&mut destination.durable).unwrap(),
            2,
            &mut &[][..],
            &Destination {
                registry: destination.storage.memory_capture.as_ref().unwrap(),
                mutations: &mutations,
                control: &control
            }
        )
        .is_err());
    assert_eq!(control.memory().used(), 0);
}
