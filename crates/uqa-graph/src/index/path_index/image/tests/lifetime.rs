//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn imported_path_generations_retain_exact_capacity_through_clones_and_captures() {
    let expected = [path(
        &[&"long".repeat(8192)],
        &(0..20000).map(|id| (id, id + 1)).collect::<Vec<_>>(),
    )];
    let bytes = frame(&expected);
    let control = StorageReadControl::with_limit(2 << 20);
    let mut restored = None;
    let allocation = allocation_counter::measure(|| {
        restored = Some(PathIndex::read_image(&mut bytes.as_slice(), &control).unwrap());
    });
    let restored = restored.unwrap();
    let retained = restored.memory_paths().unwrap();
    let mut capacity =
        size_of::<MemoryPaths>() + retained.image.paths.capacity() * size_of::<CapturedPath>();
    for path in &retained.image.paths {
        capacity += path.sequence.capacity() * size_of::<String>();
        capacity += path.pairs.capacity() * size_of::<(u64, u64)>();
        capacity += path.sequence.iter().map(String::capacity).sum::<usize>();
    }
    assert_eq!(
        allocation.bytes_current as usize,
        capacity + 2 * size_of::<usize>()
    );
    assert_eq!(control.memory().used(), capacity);
    assert_eq!(retained.retained_bytes(), capacity);
    let weak = Arc::downgrade(retained);
    let clone = restored.clone();
    let capture_control = StorageReadControl::with_limit(2 << 20);
    let image = restored
        .capture_native_memory_with_control(&capture_control)
        .unwrap();
    drop(restored);
    assert_paths(&clone, &expected);
    assert_eq!(control.memory().used(), capacity);
    drop(clone);
    assert!(weak.upgrade().is_some());
    assert_eq!(control.memory().used(), capacity);
    let invoking = StorageReadControl::with_limit(64 << 10);
    let mut reader = image.into_image_reader(&invoking).unwrap();
    let mut actual = Vec::new();
    let mut buffer = [0; 1021];
    loop {
        let count = reader.read(&mut buffer).unwrap();
        if count == 0 {
            break;
        }
        actual.extend_from_slice(&buffer[..count]);
    }
    assert_eq!(actual, bytes);
    assert!(weak.upgrade().is_none());
    for control in [&control, &capture_control, &invoking] {
        assert_eq!(control.memory().used(), 0);
    }
}

#[test]
fn path_image_preparation_and_output_use_constant_state_without_payload_copies() {
    let mut cursor_bytes = None;
    for count in [1, 64, 4096] {
        let mut expected = (0..count)
            .map(|id| path(&[&format!("{id:04}")], &[(id, id + 1)]))
            .collect::<Vec<_>>();
        expected.push(path(
            &[&"z".repeat(65549)],
            &(0..20000).map(|id| (id, id + 1)).collect::<Vec<_>>(),
        ));
        let bytes = frame(&expected);
        let source = materialized(&expected);
        let original = StorageReadControl::with_limit(4 << 20);
        let invoking = StorageReadControl::with_limit(64 << 10);
        let snapshot = source
            .capture_native_memory_with_control(&original)
            .unwrap();
        let weak = Arc::downgrade(source.memory_paths().unwrap());
        drop(source);
        let mut reader = None;
        let allocation = allocation_counter::measure(|| {
            reader = Some(snapshot.into_image_reader(&invoking).unwrap());
            let progress = reader.as_mut().unwrap().prepare(1).unwrap();
            assert!(progress.ready);
            assert_eq!(progress.positions, 0);
        });
        assert!(allocation.bytes_total < 32 * 1024, "{allocation:?}");
        let retained = invoking.memory().used();
        assert_eq!(*cursor_bytes.get_or_insert(retained), retained);
        let mut reader = reader.unwrap();
        let mut actual = vec![0; bytes.len()];
        let allocation = allocation_counter::measure(|| {
            let mut position = 0;
            while position < actual.len() {
                let end = (position + 65536).min(actual.len());
                let count = reader.read(&mut actual[position..end]).unwrap();
                assert!(count > 0 && count <= 16 * 1024);
                position += count;
            }
            assert_eq!(reader.read(&mut [0]).unwrap(), 0);
        });
        assert_eq!(allocation.count_total, 0);
        assert_eq!(actual, bytes);
        assert!(weak.upgrade().is_none());
        assert_eq!(original.memory().used(), 0);
        assert_eq!(invoking.memory().used(), 0);
    }
}

#[test]
fn path_image_children_cross_threads_after_registry_replacement_and_retirement() {
    let expected = [path(&["edge", "edge"], &[(1, 3), (3, 1)])];
    let mut catalog = PathIndexCatalog::default();
    catalog.insert("g::paths", materialized(&expected)).unwrap();
    let original = StorageReadControl::with_limit(1 << 20);
    let captured = catalog.capture(&original).unwrap();
    let child = captured.image("g::paths").unwrap().unwrap();
    catalog.insert("g::paths", PathIndex::default()).unwrap();
    catalog.remove("g::paths").unwrap();
    drop((catalog, captured));
    let invoking = StorageReadControl::with_limit(64 << 10);
    let mut reader = child.into_image_reader(&invoking).unwrap();
    let mut prefix = [0; 7];
    assert_eq!(reader.read(&mut prefix).unwrap(), 7);
    let bytes = std::thread::spawn(move || {
        let mut bytes = prefix.to_vec();
        let mut buffer = [0; 7];
        loop {
            let count = reader.read(&mut buffer).unwrap();
            if count == 0 {
                break;
            }
            bytes.extend_from_slice(&buffer[..count]);
        }
        bytes
    })
    .join()
    .unwrap();
    assert_eq!(bytes, frame(&expected));
    assert_eq!(original.memory().used(), 0);
    assert_eq!(invoking.memory().used(), 0);
}

#[test]
fn path_image_cancellation_errors_and_unwinds_retire_owned_continuations() {
    for original_cancel in [false, true] {
        for started in [false, true] {
            let source = materialized(&[path(&[&"label".repeat(8192)], &[(1, 2)])]);
            let original = StorageReadControl::with_limit(1 << 20);
            let invoking = StorageReadControl::with_limit(64 << 10);
            let snapshot = source
                .capture_native_memory_with_control(&original)
                .unwrap();
            drop(source);
            let mut reader = snapshot.into_image_reader(&invoking).unwrap();
            if started {
                assert_eq!(reader.read(&mut [0; 1021]).unwrap(), 1021);
            }
            if original_cancel {
                original.cancellation().cancel();
            } else {
                invoking.cancellation().cancel();
            }
            assert!(matches!(
                reader.prepare(1),
                Err(StorageBackendError::Cancelled(_))
            ));
            assert_eq!(original.memory().used(), 0);
            assert_eq!(invoking.memory().used(), 0);
            assert!(matches!(
                reader.read(&mut [0]),
                Err(StorageBackendError::Other(_))
            ));
        }
    }
    for failure in 0..4 {
        let source = PathIndex::default();
        let original = StorageReadControl::with_limit(4096);
        let invoking = StorageReadControl::with_limit(if failure == 0 { 0 } else { 64 << 10 });
        let snapshot = source
            .capture_native_memory_with_control(&original)
            .unwrap();
        drop(source);
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let reader = snapshot.into_image_reader(&invoking);
            if failure == 0 {
                assert!(matches!(reader, Err(StorageBackendError::Memory(_))));
                return;
            }
            let mut reader = reader.unwrap();
            if failure == 1 {
                let occupied = invoking
                    .memory()
                    .reserve(invoking.memory().available())
                    .unwrap();
                assert!(matches!(
                    reader.prepare(1),
                    Err(StorageBackendError::Memory(_))
                ));
                drop(occupied);
            } else if failure == 2 {
                assert!(reader.prepare(0).is_err());
            } else {
                assert_eq!(reader.read(&mut [0; 7]).unwrap(), 7);
                panic!("injected owner unwind");
            }
        }));
        assert_eq!(result.is_err(), failure == 3);
        assert_eq!(original.memory().used(), 0);
        assert_eq!(invoking.memory().used(), 0);
    }
}

#[test]
fn captured_path_writes_preserve_both_cancellation_owners() {
    struct Cancel<'a> {
        control: &'a StorageReadControl,
        calls: usize,
    }
    impl Write for Cancel<'_> {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.calls += 1;
            self.control.cancellation().cancel();
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    for cancel_original in [false, true] {
        let source = materialized(&[path(&[&"large".repeat(4096)], &[(1, 2)])]);
        let original = StorageReadControl::with_limit(1 << 20);
        let invoking = StorageReadControl::with_limit(4096);
        let snapshot = source
            .capture_native_memory_with_control(&original)
            .unwrap();
        drop(source);
        let mut writer = Cancel {
            control: if cancel_original {
                &original
            } else {
                &invoking
            },
            calls: 0,
        };
        assert!(matches!(
            snapshot.write_image(&mut writer, &invoking),
            Err(StorageBackendError::Cancelled(_))
        ));
        assert_eq!(writer.calls, 1);
        drop(snapshot);
        assert_eq!(original.memory().used(), 0);
        assert_eq!(invoking.memory().used(), 0);
    }
}
