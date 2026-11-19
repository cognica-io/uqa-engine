//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Export retains the joint boundary while independent source publication advances.

use super::*;
use uqa_core::memory::BudgetedVec;
use uqa_storage::backup::{RecordImageCursor, RecordImageFamily, RecordImageStep};

fn seed(store: &RedbRecordStore, value: &[u8], setup: &StorageReadControl) -> CommitSequence {
    let transaction = store.allocate_transaction(setup).unwrap();
    store
        .commit(
            transaction,
            &PreparedRecordCommit::new(
                &[
                    RecordWrite {
                        key: b"dead",
                        expected: None,
                        value: None,
                    },
                    RecordWrite {
                        key: b"row",
                        expected: None,
                        value: Some(value),
                    },
                    RecordWrite {
                        key: b"zero",
                        expected: None,
                        value: Some(b""),
                    },
                ],
                setup,
            )
            .unwrap(),
            setup,
        )
        .unwrap()
        .sequence
}

fn replace(store: RedbRecordStore, sequence: CommitSequence) {
    let (send, receive) = std::sync::mpsc::channel();
    let writer = std::thread::spawn(move || {
        let control = StorageReadControl::with_limit(1 << 20);
        write(&store, Some(sequence), b"replacement", &control);
        observe(&store, b"ids", 100, &control);
        store.reclaim_versions(&control).unwrap();
        send.send(()).unwrap();
    });
    receive
        .recv_timeout(std::time::Duration::from_secs(30))
        .expect("export must release each physical read before returning");
    writer.join().unwrap();
}

#[test]
fn record_image_cursor_preserves_redb_records_and_watermarks_with_bounded_memory() {
    let directory = tempfile::tempdir().unwrap();
    let owner = crate::RedbStorage::open(directory.path().join("image.redb")).unwrap();
    let store = owner.record_store().unwrap();
    let setup = StorageReadControl::with_limit(32 << 20);
    let value: Vec<_> = (0..(1 << 20) + 17).map(|i| (i % 251) as u8).collect();
    let sequence = seed(&store, &value, &setup);
    observe(&store, b"ids", 42, &setup);
    let control = StorageReadControl::with_limit(2048);
    let cut = capture(&store, &control);
    let weak = Arc::downgrade(&cut);
    let mut cursor = RecordImageCursor::new(cut, &control).unwrap();
    observe(&store, b"ids", 99, &setup);
    observe(&store, b"later", 1, &setup);
    let mut output = BudgetedVec::new(control.memory());
    output.extend_from_slice(&[0; 1024]).unwrap();
    let mut found = Vec::new();
    let mut wrote = false;
    let mut steps = 0;
    loop {
        steps += 1;
        assert!(steps < 20);
        let before = cursor.visited_positions();
        let step = cursor.advance(2).unwrap();
        assert!(cursor.visited_positions() - before <= 2);
        match step {
            RecordImageStep::Complete => break,
            RecordImageStep::Pending => continue,
            RecordImageStep::Unit => (),
        }
        let unit = cursor.current_unit().unwrap();
        let watermark = 42_u64.to_be_bytes();
        let expected: &[u8] = match (unit.family, unit.key) {
            (RecordImageFamily::IdentifierWatermark, b"ids") => &watermark,
            (RecordImageFamily::Record, b"row") => &value,
            (RecordImageFamily::Record, b"zero") => b"",
            _ => panic!("post-capture, deleted or unexpected unit"),
        };
        assert_eq!(unit.decoded_bytes, expected.len());
        assert_eq!(
            unit.revision,
            (unit.family == RecordImageFamily::Record).then_some(sequence)
        );
        found.push((unit.family, unit.key.to_vec()));
        let mut offset = 0;
        while offset < expected.len() {
            let copied = cursor.read_current(&mut output).unwrap();
            assert!(copied > 0);
            assert_eq!(&output[..copied], &expected[offset..offset + copied]);
            offset += copied;
            if expected.len() > 1024 && !wrote {
                replace(store.clone(), sequence);
                wrote = true;
            }
        }
    }
    assert_eq!(
        found,
        vec![
            (RecordImageFamily::IdentifierWatermark, b"ids".to_vec()),
            (RecordImageFamily::Record, b"row".to_vec()),
            (RecordImageFamily::Record, b"zero".to_vec()),
        ]
    );
    assert!(wrote);
    assert_eq!(cursor.copied_bytes(), 8 + value.len() as u64);
    assert!(weak.upgrade().is_none());
    assert!(control.memory().peak() <= 2048);
    drop(output);
    assert_eq!(control.memory().used(), 0);
}
