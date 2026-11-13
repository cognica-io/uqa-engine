//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::image_io::{ImageDecodeWindow, ImageDecoder};

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
