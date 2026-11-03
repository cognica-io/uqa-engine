//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! The image cursor preserves every selected record and watermark while independent writers progress.

use super::*;
use std::collections::BTreeMap;
use uqa_storage::{
    backup::{RecordImageCursor, RecordImageFamily, RecordImageStep},
    mvcc::{CommittedBackupSnapshot, IdentifierRequest},
};

struct Expected {
    revision: Option<CommitSequence>,
    value: Vec<u8>,
}

fn expected_units(
    cut: &dyn CommittedBackupSnapshot,
    setup: &StorageReadControl,
) -> BTreeMap<(bool, Vec<u8>), Expected> {
    let mut expected = BTreeMap::new();
    cut.records()
        .visit_prefix(b"", None, usize::MAX, setup, &mut |key, record| {
            if let Some(value) = record.value {
                expected.insert(
                    (true, key.to_vec()),
                    Expected {
                        revision: record.revision,
                        value: value.to_vec(),
                    },
                );
            }
            Ok(true)
        })
        .unwrap();
    cut.visit_identifier_watermarks(None, usize::MAX, setup, &mut |key, value| {
        if let Some(value) = value {
            expected.insert(
                (false, key.to_vec()),
                Expected {
                    revision: None,
                    value: value.to_be_bytes().to_vec(),
                },
            );
        }
        Ok(true)
    })
    .unwrap();
    expected
}

fn verify_image(mode: usize, native: bool) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("image.db");
    let setup = StorageReadControl::with_limit(32 << 20);
    let definition = format!("{{\"value\":\"{}\"}}", "x".repeat((512 << 10) + 17));
    let (store, key) = open_source(&path, mode, native, &definition, &setup);
    store
        .allocate_identifiers(b"image.ids", IdentifierRequest::Observe(41), &setup)
        .unwrap();
    let control = StorageReadControl::with_limit(4096);
    let cut = store.capture_backup_snapshot(&control).unwrap().unwrap();
    let mut expected = expected_units(cut.as_ref(), &setup);
    assert!(expected.get(&(true, key)).unwrap().value.len() > 512 << 10);
    assert_eq!(
        expected.get(&(false, b"image.ids".to_vec())).unwrap().value,
        41_u64.to_be_bytes()
    );
    let total: usize = expected.values().map(|unit| unit.value.len()).sum();
    let weak = Arc::downgrade(&cut);
    let mut cursor = RecordImageCursor::new(cut, &control).unwrap();
    for namespace in [b"image.ids".as_slice(), b"future.ids"] {
        store
            .allocate_identifiers(namespace, IdentifierRequest::Observe(99), &setup)
            .unwrap();
    }
    let mut output = BudgetedVec::new(control.memory());
    output.extend_from_slice(&[0; 1024]).unwrap();
    let mut wrote = false;
    let mut steps = 0;
    loop {
        steps += 1;
        assert!(
            steps < 1000,
            "image traversal must complete on the fixed fixture"
        );
        let before = cursor.visited_positions();
        let step = cursor.advance(2).unwrap();
        assert!(cursor.visited_positions() - before <= 2);
        match step {
            RecordImageStep::Complete => break,
            RecordImageStep::Pending => continue,
            RecordImageStep::Unit => (),
        }
        let unit = cursor.current_unit().unwrap();
        let selected = expected
            .remove(&(unit.family == RecordImageFamily::Record, unit.key.to_vec()))
            .expect("each captured unit is emitted exactly once");
        assert_eq!(unit.revision, selected.revision);
        assert_eq!(unit.decoded_bytes, selected.value.len());
        let mut offset = 0;
        while offset < selected.value.len() {
            let copied = cursor.read_current(&mut output).unwrap();
            assert!(copied > 0);
            assert_eq!(&output[..copied], &selected.value[offset..offset + copied]);
            offset += copied;
            if selected.value.len() > 1024 && !wrote {
                replace_on_another_connection(&path, mode, native);
                store.reclaim_versions(&setup).unwrap();
                wrote = true;
            }
        }
    }
    assert!(wrote);
    assert!(expected.is_empty());
    assert_eq!(cursor.copied_bytes(), total as u64);
    assert!(weak.upgrade().is_none());
    assert!(control.memory().peak() <= 4096);
    drop(output);
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn record_image_cursor_preserves_native_and_kv_cuts_in_every_file_mode() {
    for mode in 0..4 {
        for native in [false, true] {
            verify_image(mode, native);
        }
    }
}

#[test]
fn record_image_cursor_includes_compacted_run_keys_and_completes_empty_sources() {
    let store = SQLiteRecordStore::new(&ManagedConnection::open_in_memory().unwrap()).unwrap();
    let setup = StorageReadControl::with_limit(8 << 20);
    let control = StorageReadControl::with_limit(4096);
    let empty = store.capture_backup_snapshot(&control).unwrap().unwrap();
    let mut cursor = RecordImageCursor::new(empty, &control).unwrap();
    assert_eq!(cursor.advance(1).unwrap(), RecordImageStep::Pending);
    assert_eq!(cursor.advance(1).unwrap(), RecordImageStep::Complete);
    assert_eq!(cursor.visited_positions(), 0);
    assert_eq!(control.memory().used(), 0);

    let keys: Vec<_> = (0..128_u64).map(u64::to_be_bytes).collect();
    let writes: Vec<_> = keys
        .iter()
        .map(|key| RecordWrite {
            key,
            expected: None,
            value: Some(b"template"),
        })
        .collect();
    let transaction = store.allocate_transaction(&setup).unwrap();
    let sequence = store
        .commit(
            transaction,
            &PreparedRecordCommit::new(&writes, &setup).unwrap(),
            &setup,
        )
        .unwrap()
        .sequence;
    store.reclaim_versions(&setup).unwrap();
    assert!(
        store
            .with(|connection| Ok(connection.query_row(
                "SELECT count(*) FROM _uqa_mvcc_runs",
                [],
                |row| row.get::<_, i64>(0)
            )?))
            .unwrap()
            > 0
    );
    let cut = store.capture_backup_snapshot(&control).unwrap().unwrap();
    let mut cursor = RecordImageCursor::new(cut, &control).unwrap();
    let mut count = 0_u64;
    loop {
        match cursor.advance(1).unwrap() {
            RecordImageStep::Complete => break,
            RecordImageStep::Pending => continue,
            RecordImageStep::Unit => (),
        }
        let unit = cursor.current_unit().unwrap();
        assert_eq!(unit.family, RecordImageFamily::Record);
        assert_eq!(unit.key, count.to_be_bytes());
        assert_eq!(unit.revision, Some(sequence));
        assert_eq!(unit.decoded_bytes, 8);
        let mut output = [0; 8];
        assert_eq!(cursor.read_current(&mut output[..3]).unwrap(), 3);
        assert_eq!(cursor.read_current(&mut output[3..]).unwrap(), 5);
        assert_eq!(output, *b"template");
        count += 1;
    }
    assert_eq!(count, 128);
    assert_eq!(cursor.copied_bytes(), 128 * 8);
    assert_eq!(control.memory().used(), 0);
}
