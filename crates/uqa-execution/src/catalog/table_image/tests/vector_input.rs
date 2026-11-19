//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

struct NameWire<'a> {
    source: std::io::Cursor<&'a [u8]>,
    end_reads: usize,
}

impl std::io::Read for NameWire<'_> {
    fn read(&mut self, bytes: &mut [u8]) -> std::io::Result<usize> {
        assert_eq!(
            self.end_reads, 0,
            "registration reread completed vector input"
        );
        let count = self.source.read(bytes)?;
        self.end_reads += usize::from(count == 0);
        Ok(count)
    }
}

#[test]
fn vector_field_admission_and_registration_resume_without_source_replay() {
    let units = fixture(0);
    let field = name(&"é\0embedding".repeat(16384));
    let selected = DecodedMemoryTableUnit::Vector(field.clone());
    let control = StorageReadControl::with_limit(8 << 20);
    let registry = MemoryCaptureRegistry::new();
    let mut input = builder(&control, &registry);
    for (unit, bytes) in &units[..2] {
        input = input.read(unit, &mut bytes.as_slice()).unwrap();
    }
    let mut wire = NameWire {
        source: std::io::Cursor::new(&units[2].1),
        end_reads: 0,
    };
    let mut before_reads = 0;
    let mut after_end = 0;
    loop {
        let position = wire.source.position();
        let (next, complete) = input
            .advance(
                &selected,
                &mut wire,
                ImageDecodeWindow {
                    bytes: 17,
                    steps: 1,
                },
            )
            .unwrap();
        input = next;
        assert!(wire.source.position() - position <= 17);
        before_reads += usize::from(wire.source.position() == 0);
        after_end += usize::from(wire.end_reads == 1);
        if complete {
            break;
        }
    }
    assert!(before_reads >= field.len() / 4096);
    assert!(after_end >= field.len() / 4096);
    assert_eq!(wire.end_reads, 1);
    let data = input.finish().unwrap();
    control.cancellation().cancel();
    let output = StorageReadControl::with_limit(1 << 20);
    let mut bytes = Vec::new();
    data.vectors
        .get(field.as_ref())
        .unwrap()
        .write_image(&mut bytes, &output)
        .unwrap();
    assert_eq!(bytes, units[2].1);
    drop((data, registry));
    assert_eq!(control.memory().used(), 0);
    assert_eq!(output.memory().used(), 0);
}

#[test]
fn unfinished_vector_name_and_source_registration_retire_the_entire_table() {
    let units = fixture(0);
    let selected = DecodedMemoryTableUnit::Vector(name(&"é\0embedding".repeat(16384)));
    for registration in [false, true] {
        for failure in ["drop", "cancel", "memory"] {
            let control = StorageReadControl::with_limit(8 << 20);
            let registry = MemoryCaptureRegistry::new();
            let mut input = builder(&control, &registry);
            for (unit, bytes) in &units[..2] {
                input = input.read(unit, &mut bytes.as_slice()).unwrap();
            }
            let mut wire = NameWire {
                source: std::io::Cursor::new(&units[2].1),
                end_reads: 0,
            };
            loop {
                let (next, complete) = input
                    .advance(
                        &selected,
                        &mut wire,
                        ImageDecodeWindow {
                            bytes: 17,
                            steps: 1,
                        },
                    )
                    .unwrap();
                input = next;
                assert!(!complete);
                if !registration || wire.end_reads == 1 {
                    break;
                }
            }
            if failure == "drop" {
                drop(input);
            } else {
                if failure == "cancel" {
                    control.cancellation().cancel();
                }
                let held = (failure == "memory").then(|| {
                    control
                        .memory()
                        .reserve(control.memory().available())
                        .unwrap()
                });
                loop {
                    match input.advance(
                        &selected,
                        &mut wire,
                        ImageDecodeWindow {
                            bytes: 17,
                            steps: 1,
                        },
                    ) {
                        Ok((next, false)) => input = next,
                        Ok((_, true)) => panic!("failed registration published a table"),
                        Err(_) => break,
                    }
                }
                drop(held);
            }
            drop(registry);
            assert_eq!(control.memory().used(), 0, "{registration} {failure}");
        }
    }
}

#[test]
fn table_vector_input_preserves_current_windows_all_index_kinds_and_later_writes() {
    for kind in 0..4 {
        let units = fixture(kind);
        let control = StorageReadControl::with_limit(32 << 20);
        let registry = MemoryCaptureRegistry::new();
        let mut input = builder(&control, &registry);
        for (unit, bytes) in &units {
            let mut reader = std::io::Cursor::new(bytes);
            let mut calls = 0;
            loop {
                calls += 1;
                let before = reader.position();
                let window = ImageDecodeWindow {
                    bytes: if calls % 2 == 0 { 1 } else { 17 },
                    steps: 1,
                };
                let (next, complete) = input.advance(unit, &mut reader, window).unwrap();
                input = next;
                assert!(reader.position() - before <= window.bytes as u64);
                if complete {
                    break;
                }
            }
            assert!(calls > 1);
        }
        let mut data = input.finish().unwrap();
        assert!(control.memory().used() > 0);
        let vectors = registry.cell(std::mem::take(&mut data.vectors)).unwrap();
        control.cancellation().cancel();
        let mut actual = vec![];
        let output = StorageReadControl::with_limit(32 << 20);
        vectors
            .read()
            .get("embedding")
            .unwrap()
            .write_image(&mut actual, &output)
            .unwrap();
        assert_eq!(actual, units[2].1);
        let snapshot = vectors.read().get("embedding").unwrap().snapshot().unwrap();
        let cut = registry.capture(&output).unwrap();
        let selected = vectors.captured(&cut).unwrap();
        let capture = selected.image("embedding", &cut).unwrap();
        vectors
            .write()
            .live_mut()
            .unwrap()
            .get_mut("embedding")
            .unwrap()
            .add(17, vec![-1.0, 0.0])
            .unwrap();
        assert!(vectors
            .read()
            .get("embedding")
            .unwrap()
            .contains_document(17)
            .unwrap());
        assert!(!snapshot.contains_document(17).unwrap());
        drop((data, snapshot, vectors, selected, cut, registry));
        // A capture may retain the original allocation or own an admitted copy.
        assert!(output.memory().used() > 0);
        let mut historical = vec![];
        capture.write_image(&mut historical, &output).unwrap();
        assert_eq!(historical, units[2].1);
        drop(capture);
        assert_eq!(control.memory().used(), 0);
        assert_eq!(output.memory().used(), 0);
    }
}

#[test]
fn table_vector_input_rejects_changed_selection_and_retires_incomplete_tables() {
    for kind in 0..4 {
        let units = fixture(kind);
        for failure in [
            "field", "document", "text", "value", "finish", "cancel", "window", "memory", "drop",
        ] {
            let control = StorageReadControl::with_limit(32 << 20);
            let registry = MemoryCaptureRegistry::new();
            let mut input = builder(&control, &registry);
            for (unit, bytes) in &units[..2] {
                input = input.read(unit, &mut bytes.as_slice()).unwrap();
            }
            let mut reader = std::io::Cursor::new(&units[2].1);
            let (input, complete) = input
                .advance(
                    &units[2].0,
                    &mut reader,
                    ImageDecodeWindow {
                        bytes: 1,
                        steps: 64,
                    },
                )
                .unwrap();
            assert!(!complete);
            assert_eq!(reader.position(), 1);
            match failure {
                "drop" => drop(input),
                "finish" => assert!(input.finish().is_err()),
                _ => {
                    if failure == "cancel" {
                        control.cancellation().cancel();
                    }
                    let held = (failure == "memory").then(|| {
                        control
                            .memory()
                            .reserve(control.memory().available())
                            .unwrap()
                    });
                    let other = DecodedMemoryTableUnit::Vector(name("different"));
                    let unit = match failure {
                        "field" => &other,
                        "document" => &units[0].0,
                        "text" => &units[1].0,
                        "value" => &units[3].0,
                        _ => &units[2].0,
                    };
                    let mut input = input;
                    loop {
                        match input.advance(
                            unit,
                            &mut reader,
                            ImageDecodeWindow {
                                bytes: 17,
                                steps: usize::from(failure != "window"),
                            },
                        ) {
                            Ok((next, false)) => input = next,
                            Ok((_, true)) => panic!("invalid selected vector completed: {failure}"),
                            Err(_) => break,
                        }
                    }
                    drop(held);
                }
            }
            assert_eq!(control.memory().used(), 0, "{kind}: {failure}");
        }
    }
}
