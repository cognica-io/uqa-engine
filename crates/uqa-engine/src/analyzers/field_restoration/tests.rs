//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::image_io::{ImageDecodeWindow, ImageDecoder};
use uqa_storage::read_control::StorageReadControl;

#[test]
fn gin_field_publication_preserves_native_resource_errors_and_selected_generation() {
    let engine = Engine::new();
    engine.sql("CREATE TABLE docs(body text)", &[]).unwrap();
    let table = Table(engine.try_table("docs").unwrap().unwrap());
    table.register_field("previous").unwrap();
    let before = table.fields();
    for cancelled in [false, true] {
        let control = StorageReadControl::with_limit(32768);
        let target = table.clone();
        let mut input = ImageDecoder::<(), StorageBackendError>::new(
            control.memory(),
            control.cancellation(),
            |mut reader| async move {
                target
                    .register_field_from("body", &mut reader.input())
                    .await
            },
        )
        .unwrap();
        let window = ImageDecodeWindow { bytes: 1, steps: 1 };
        assert!(input
            .advance(&mut std::io::empty(), window)
            .unwrap()
            .is_none());
        let occupied = (!cancelled).then(|| {
            control
                .memory()
                .reserve(control.memory().available())
                .unwrap()
        });
        if cancelled {
            control.cancellation().cancel();
        }
        let error = loop {
            match input.advance(&mut std::io::empty(), window) {
                Err(error) => break error,
                Ok(None) => (),
                Ok(Some(())) => panic!("interrupted field publication completed"),
            }
        };
        let error = uqa_storage::backup::BackupError::from(error);
        assert!(matches!(
            (cancelled, error),
            (true, uqa_storage::backup::BackupError::Cancelled(_))
                | (false, uqa_storage::backup::BackupError::Memory(_))
        ));
        assert!(table.fields().shares_definition(&before));
        assert_eq!(table.fields().as_slice(), ["previous"]);
        assert!(input.advance(&mut std::io::empty(), window).is_err());
        drop((input, occupied));
        assert_eq!(control.memory().used(), 0);
    }
    table.register_field("later").unwrap();
    assert_eq!(before.as_slice(), ["previous"]);
    assert_eq!(table.fields().as_slice(), ["previous", "later"]);
}

#[test]
fn retained_gin_field_publication_keeps_predecessors_and_independent_later_writes() {
    let engine = Engine::new();
    engine.sql("CREATE TABLE docs(body text)", &[]).unwrap();
    let table = Table(engine.try_table("docs").unwrap().unwrap());
    let before = table.fields();
    assert!(before.is_empty());
    let control = StorageReadControl::with_limit(32768);
    let target = table.clone();
    let mut input = ImageDecoder::<(), StorageBackendError>::new(
        control.memory(),
        control.cancellation(),
        |mut reader| async move {
            target
                .register_field_from("body", &mut reader.input())
                .await
        },
    )
    .unwrap();
    assert!(input
        .advance(
            &mut std::io::empty(),
            ImageDecodeWindow { bytes: 1, steps: 1 }
        )
        .unwrap()
        .is_none());
    assert!(table.fields().is_empty());
    loop {
        if input
            .advance(
                &mut std::io::empty(),
                ImageDecodeWindow { bytes: 1, steps: 1 },
            )
            .unwrap()
            .is_some()
        {
            break;
        }
    }
    let retained = table.fields();
    assert_eq!(retained.as_slice(), ["body"]);
    assert!(before.is_empty());
    let occupied = control
        .memory()
        .reserve(control.memory().available())
        .unwrap();
    control.cancellation().cancel();
    table.register_field("later").unwrap();
    assert_eq!(table.fields().as_slice(), ["body", "later"]);
    assert_eq!(retained.as_slice(), ["body"]);
    drop((engine, table, retained, before, input, occupied));
    assert_eq!(control.memory().used(), 0);
}
