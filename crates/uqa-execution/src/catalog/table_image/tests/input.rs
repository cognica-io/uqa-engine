//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn table_text_input_rejects_address_changes_and_incomplete_publication() {
    let units = fixture(0);
    for failure in [
        "identity", "document", "vector", "value", "finish", "cancel", "window", "memory", "drop",
    ] {
        let control = StorageReadControl::with_limit(32 << 20);
        let registry = MemoryCaptureRegistry::new();
        let input = builder(&control, &registry)
            .read(&units[0].0, &mut units[0].1.as_slice())
            .unwrap();
        let mut reader = std::io::Cursor::new(&units[1].1);
        let (input, complete) = input
            .advance(
                &units[1].0,
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
            "finish" => {
                assert!(input.finish().is_err());
            }
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
                let identity = DecodedMemoryTableUnit::Text(9);
                let unit = match failure {
                    "identity" => &identity,
                    "document" => &units[0].0,
                    "vector" => &units[2].0,
                    "value" => &units[3].0,
                    _ => &units[1].0,
                };
                let mut input = input;
                loop {
                    match input.advance(
                        unit,
                        &mut reader,
                        ImageDecodeWindow {
                            bytes: 1,
                            steps: usize::from(failure != "window"),
                        },
                    ) {
                        Ok((next, false)) => input = next,
                        Ok((_, true)) => panic!("incomplete text published: {failure}"),
                        Err(_) => break,
                    }
                }
                drop(held);
            }
        }
        assert_eq!(control.memory().used(), 0, "{failure}");
    }
}

#[test]
fn table_text_input_keeps_current_windows_posting_images_and_later_mutations() {
    let units = fixture(0);
    let control = StorageReadControl::with_limit(32 << 20);
    let registry = MemoryCaptureRegistry::new();
    let mut input = builder(&control, &registry);
    let mut post_input = 0;
    for (unit, bytes) in &units {
        let mut reader = std::io::Cursor::new(bytes);
        let mut calls = 0;
        loop {
            calls += 1;
            let window = ImageDecodeWindow {
                bytes: if calls % 2 == 0 { 1 } else { 17 },
                steps: 1,
            };
            let before = reader.position();
            let text = matches!(unit, DecodedMemoryTableUnit::Text(_));
            post_input += usize::from(text && before == bytes.len() as u64);
            let (next, complete) = input.advance(unit, &mut reader, window).unwrap();
            input = next;
            if text {
                assert!(reader.position() - before <= window.bytes as u64);
            }
            if complete {
                break;
            }
        }
    }
    assert!(post_input > 6);
    let mut data = input.finish().unwrap();
    control.cancellation().cancel();
    let snapshot = data.text.snapshot().unwrap();
    let output = StorageReadControl::with_limit(1 << 20);
    let mut bytes = vec![];
    snapshot
        .write_image_document(7, &mut bytes, &output)
        .unwrap();
    assert_eq!(bytes, units[1].1);
    data.text
        .add_document(7, BTreeMap::from([("body".into(), "later later".into())]))
        .unwrap();
    assert_eq!(data.text.get_term_freq(7, "body", "later").unwrap(), 2);
    assert_eq!(snapshot.get_term_freq(7, "body", "original").unwrap(), 1);
    drop(data);
    assert!(control.memory().used() > 0);
    drop(snapshot);
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn table_evaluated_input_rejects_address_changes_and_incomplete_publication() {
    let units = fixture(0);
    for selected in [3, 4] {
        for failure in [
            "identity",
            "name",
            "component",
            "finish",
            "cancel",
            "memory",
            "window",
            "drop",
        ] {
            let control = StorageReadControl::with_limit(32 << 20);
            let registry = MemoryCaptureRegistry::new();
            let mut input = builder(&control, &registry);
            for (unit, bytes) in &units[..selected] {
                input = input.read(unit, &mut bytes.as_slice()).unwrap();
            }
            let (unit, bytes) = &units[selected];
            let mut reader = std::io::Cursor::new(bytes);
            let (input, complete) = input
                .advance(
                    unit,
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
                "finish" => {
                    assert!(input.finish().is_err());
                }
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
                    let changed = match failure {
                        "identity" => DecodedMemoryTableUnit::ValueIndex {
                            name: name("opaque\0key"),
                            document: Some(9),
                        },
                        "name" => DecodedMemoryTableUnit::ValueIndex {
                            name: name("different"),
                            document: (selected == 4).then_some(7),
                        },
                        _ => DecodedMemoryTableUnit::Document(7),
                    };
                    let unit = if matches!(failure, "identity" | "name" | "component") {
                        &changed
                    } else {
                        unit
                    };
                    let mut input = input;
                    loop {
                        match input.advance(
                            unit,
                            &mut reader,
                            ImageDecodeWindow {
                                bytes: 1,
                                steps: usize::from(failure != "window"),
                            },
                        ) {
                            Ok((next, false)) => input = next,
                            Ok((_, true)) => {
                                panic!("failed index input published: {selected} {failure}")
                            }
                            Err(_) => break,
                        }
                    }
                    drop(held);
                }
            }
            assert_eq!(control.memory().used(), 0, "{selected} {failure}");
        }
    }
}

#[test]
fn table_evaluated_input_preserves_each_calls_window_and_retained_values() {
    let mut units = fixture(0);
    let value = Value::List(vec![Value::Str("한🙂".repeat(4096)); 4]);
    let output = StorageReadControl::with_limit(8 << 20);
    units[5].1.clear();
    write_value_image(
        &value,
        &mut units[5].1,
        output.memory(),
        output.cancellation(),
    )
    .unwrap();
    let control = StorageReadControl::with_limit(32 << 20);
    let registry = MemoryCaptureRegistry::new();
    let mut input = builder(&control, &registry);
    let mut post_input = 0;
    for (unit, bytes) in &units {
        let mut reader = std::io::Cursor::new(bytes);
        let mut calls = 0;
        loop {
            calls += 1;
            let window = ImageDecodeWindow {
                bytes: if calls % 2 == 0 { 1 } else { 17 },
                steps: 1,
            };
            let before = reader.position();
            let evaluated = matches!(unit, DecodedMemoryTableUnit::ValueIndex { .. });
            if evaluated && before == bytes.len() as u64 {
                post_input += 1;
            }
            let (next, complete) = input.advance(unit, &mut reader, window).unwrap();
            input = next;
            if evaluated {
                assert!(reader.position() - before <= window.bytes as u64);
            }
            if complete {
                break;
            }
        }
        assert_eq!(reader.position(), bytes.len() as u64);
    }
    assert!(post_input > 30);
    drop(units);
    let mut data = input.finish().unwrap();
    control.cancellation().cancel();
    let key = ValueIndexKey::Index("opaque\0key".into());
    let index = data.values.get(&key).unwrap();
    assert_eq!(
        index.stored_value(9, &output).unwrap().as_deref(),
        Some(&value)
    );
    let retained = index.capture_values(&output).unwrap();
    drop(index);
    data.values
        .get_mut(&key)
        .unwrap()
        .replace(9, Some(&Value::Int(42)), &output)
        .unwrap();
    assert_eq!(&**retained.value(9).unwrap().as_ref().unwrap(), &value);
    assert_eq!(
        data.values
            .get(&key)
            .unwrap()
            .stored_value(9, &output)
            .unwrap()
            .as_deref(),
        Some(&Value::Int(42))
    );
    drop((data, registry));
    assert!(control.memory().used() > 0);
    drop(retained);
    assert_eq!(control.memory().used(), 0);
    assert_eq!(output.memory().used(), 0);
}

#[test]
fn table_document_input_rejects_address_changes_and_incomplete_publication() {
    let units = fixture(0);
    for failure in [
        "identity",
        "component",
        "finish",
        "cancel",
        "window",
        "drop",
    ] {
        let control = StorageReadControl::with_limit(32 << 20);
        let registry = MemoryCaptureRegistry::new();
        let mut reader = std::io::Cursor::new(&units[0].1);
        let (input, complete) = builder(&control, &registry)
            .advance(
                &units[0].0,
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
            "finish" => {
                assert!(input.finish().is_err());
            }
            _ => {
                if failure == "cancel" {
                    control.cancellation().cancel();
                }
                let unit = match failure {
                    "identity" => &DecodedMemoryTableUnit::Document(9),
                    "component" => &units[1].0,
                    _ => &units[0].0,
                };
                assert!(input
                    .advance(
                        unit,
                        &mut reader,
                        ImageDecodeWindow {
                            bytes: 1,
                            steps: if failure == "window" { 0 } else { 64 }
                        }
                    )
                    .is_err());
            }
        }
        assert_eq!(control.memory().used(), 0, "{failure}");
    }
}

#[test]
fn table_document_input_retains_each_calls_window_and_original_data() {
    let units = fixture(0);
    let control = StorageReadControl::with_limit(32 << 20);
    let registry = MemoryCaptureRegistry::new();
    let mut input = builder(&control, &registry);
    for (unit, bytes) in &units {
        let mut reader = std::io::Cursor::new(bytes);
        let mut calls = 0;
        loop {
            calls += 1;
            let window = ImageDecodeWindow {
                bytes: if calls % 2 == 0 { 1 } else { 7 },
                steps: 1,
            };
            let before = reader.position();
            let (next, complete) = input.advance(unit, &mut reader, window).unwrap();
            input = next;
            if matches!(unit, DecodedMemoryTableUnit::Document(_)) {
                assert!(reader.position() - before <= window.bytes as u64);
            }
            if complete {
                break;
            }
        }
        assert_eq!(reader.position(), bytes.len() as u64);
    }
    drop(units);
    let mut data = input.finish().unwrap();
    control.cancellation().cancel();
    assert_eq!(
        data.documents.get_field(7, "body").unwrap(),
        Some(Value::Str("original words".into()))
    );
    data.documents
        .put(8, BTreeMap::from([("body".into(), Value::Int(42))]))
        .unwrap();
    assert_eq!(
        data.documents.get_field(8, "body").unwrap(),
        Some(Value::Int(42))
    );
    drop(data);
    assert_eq!(control.memory().used(), 0);
}
