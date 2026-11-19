//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

struct Wire<'a> {
    source: std::io::Cursor<&'a [u8]>,
    end_reads: usize,
}

impl std::io::Read for Wire<'_> {
    fn read(&mut self, bytes: &mut [u8]) -> std::io::Result<usize> {
        assert_eq!(
            self.end_reads, 0,
            "registration replayed completed value input"
        );
        let count = self.source.read(bytes)?;
        self.end_reads += usize::from(count == 0);
        Ok(count)
    }
}

fn named_units(count: usize) -> Vec<Unit> {
    let mut units = fixture(0);
    units[3].1[12..20].copy_from_slice(&(count as u64).to_le_bytes());
    units.truncate(4 + count);
    let index_name = name(&"é\0evaluated".repeat(16384));
    for (unit, _) in &mut units[3..] {
        let DecodedMemoryTableUnit::ValueIndex { name, .. } = unit else {
            unreachable!()
        };
        *name = index_name.clone();
    }
    units
}

fn decode_named(
    units: &[Unit],
    control: &StorageReadControl,
    registry: &MemoryCaptureRegistry,
) -> (MemoryTableData, usize, usize) {
    let mut input = builder(control, registry);
    for (unit, bytes) in &units[..3] {
        input = input.read(unit, &mut bytes.as_slice()).unwrap();
    }
    let mut name_polls = 0;
    let mut registration_polls = 0;
    for (ordinal, (unit, bytes)) in units[3..].iter().enumerate() {
        let mut wire = Wire {
            source: std::io::Cursor::new(bytes.as_slice()),
            end_reads: 0,
        };
        let mut calls = 0;
        loop {
            calls += 1;
            let window = ImageDecodeWindow {
                bytes: if calls % 2 == 0 { 1 } else { 17 },
                steps: 1,
            };
            let position = wire.source.position();
            let (next, complete) = input.advance(unit, &mut wire, window).unwrap();
            input = next;
            assert!(wire.source.position() - position <= window.bytes as u64);
            name_polls += usize::from(ordinal == 0 && wire.source.position() == 0);
            registration_polls += usize::from(ordinal + 4 == units.len() && wire.end_reads == 1);
            if complete {
                break;
            }
        }
        assert_eq!(wire.end_reads, 1);
    }
    (input.finish().unwrap(), name_polls, registration_polls)
}

#[test]
fn named_value_input_resumes_name_admission_and_empty_or_last_value_registration() {
    for count in [0, 2] {
        let units = named_units(count);
        let DecodedMemoryTableUnit::ValueIndex {
            name: index_name, ..
        } = &units[3].0
        else {
            unreachable!()
        };
        let control = StorageReadControl::with_limit(8 << 20);
        let output = StorageReadControl::with_limit(8 << 20);
        let registry = MemoryCaptureRegistry::new();
        let (mut data, name_polls, registration_polls) = decode_named(&units, &control, &registry);
        assert!(name_polls >= index_name.len() / 4096);
        assert!(registration_polls >= index_name.len() / 4096);
        let key = ValueIndexKey::Index(index_name.to_string());
        let captured = data
            .values
            .get(&key)
            .unwrap()
            .capture_values(&output)
            .unwrap();
        let mut metadata = Vec::new();
        let mut image = captured.metadata_image_reader(&output).unwrap();
        loop {
            let mut buffer = [0; 17];
            let count = image.read(&mut buffer).unwrap();
            if count == 0 {
                break;
            }
            metadata.extend_from_slice(&buffer[..count]);
        }
        drop(image);
        assert_eq!(metadata, units[3].1);
        assert_eq!(captured.len().unwrap(), count);
        if count != 0 {
            assert_eq!(captured.value(7).unwrap().as_deref(), Some(&Value::Null));
            assert_eq!(
                captured.value(9).unwrap().as_deref(),
                Some(&Value::Str("stored expression".into()))
            );
        }
        let source = registry.cell(data.values).unwrap();
        let cut = registry.capture(&output).unwrap();
        let names = source.captured(&cut).unwrap();
        let child = names.get(&key, &cut).unwrap().unwrap();
        control.cancellation().cancel();
        let occupied = control
            .memory()
            .reserve(control.memory().available())
            .unwrap();
        source
            .write()
            .get_mut(&key)
            .unwrap()
            .replace(99, Some(&Value::Int(42)), &output)
            .unwrap();
        assert!(child.value(99).unwrap().is_none());
        assert_eq!(
            source
                .read()
                .get(&key)
                .unwrap()
                .stored_value(99, &output)
                .unwrap()
                .as_deref(),
            Some(&Value::Int(42))
        );
        // Other reconstructed owners and their later writes remain independent of index registration.
        data.documents
            .put(19, BTreeMap::from([("body".into(), Value::Int(19))]))
            .unwrap();
        drop((
            data.documents,
            data.text,
            data.vectors,
            source,
            names,
            cut,
            registry,
            units,
        ));
        if count == 0 {
            // Capturing an empty population retains no original value nodes; its copied field belongs to the capture allowance.
            assert_eq!(control.memory().used(), occupied.bytes());
        } else {
            assert!(control.memory().used() > occupied.bytes());
        }
        drop((captured, child));
        assert_eq!(control.memory().used(), occupied.bytes());
        drop(occupied);
        assert_eq!(control.memory().used(), 0);
        assert_eq!(output.memory().used(), 0);
    }
}

#[test]
fn unfinished_value_name_and_registration_retire_the_whole_table() {
    for count in [0, 2] {
        let units = named_units(count);
        for registration in [false, true] {
            for failure in ["drop", "cancel", "memory", "window", "address", "finish"] {
                let control = StorageReadControl::with_limit(8 << 20);
                let registry = MemoryCaptureRegistry::new();
                let mut input = builder(&control, &registry);
                let selected = if registration { 3 + count } else { 3 };
                for (unit, bytes) in &units[..selected] {
                    input = input.read(unit, &mut bytes.as_slice()).unwrap();
                }
                let (unit, bytes) = &units[selected];
                let mut wire = Wire {
                    source: std::io::Cursor::new(bytes.as_slice()),
                    end_reads: 0,
                };
                loop {
                    let (next, complete) = input
                        .advance(
                            unit,
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
                        let changed = DecodedMemoryTableUnit::ValueIndex {
                            name: name("another"),
                            document: None,
                        };
                        let unit = if failure == "address" { &changed } else { unit };
                        loop {
                            match input.advance(unit, &mut wire, ImageDecodeWindow { bytes: 17, steps: usize::from(failure != "window") }) {
                                Ok((next, false)) => input = next,
                                Ok((_, true)) => panic!("incomplete registration published: {count} {registration} {failure}"),
                                Err(_) => break,
                            }
                        }
                        drop(held);
                    }
                }
                drop(registry);
                assert_eq!(
                    control.memory().used(),
                    0,
                    "{count} {registration} {failure}"
                );
            }
        }
    }
}

#[test]
fn named_index_membership_rejects_missing_excess_and_repeated_definitions() {
    let units = fixture(0);
    for failure in ["missing", "next", "duplicate", "excess", "trailing"] {
        let control = StorageReadControl::with_limit(8 << 20);
        let registry = MemoryCaptureRegistry::new();
        let mut input = builder(&control, &registry);
        let end = if matches!(failure, "missing" | "next" | "trailing") {
            5
        } else {
            6
        };
        for (unit, bytes) in &units[..end] {
            input = input.read(unit, &mut bytes.as_slice()).unwrap();
        }
        match failure {
            "missing" => assert!(input.finish().is_err()),
            "next" | "duplicate" => {
                let next = DecodedMemoryTableUnit::ValueIndex {
                    name: name(if failure == "next" {
                        "another"
                    } else {
                        "opaque\0key"
                    }),
                    document: None,
                };
                // An empty definition must still reject a duplicate registration.
                let mut metadata = units[3].1.clone();
                metadata[12..20].copy_from_slice(&0_u64.to_le_bytes());
                assert!(input.read(&next, &mut metadata.as_slice()).is_err());
            }
            "excess" => assert!(input.read(&units[5].0, &mut units[5].1.as_slice()).is_err()),
            "trailing" => {
                let mut bytes = units[5].1.clone();
                bytes.push(0);
                assert!(input.read(&units[5].0, &mut bytes.as_slice()).is_err());
            }
            _ => unreachable!(),
        }
        assert_eq!(control.memory().used(), 0, "{failure}");
    }
}
