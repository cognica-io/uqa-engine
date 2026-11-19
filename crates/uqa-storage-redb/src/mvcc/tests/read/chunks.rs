//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::memory::BudgetedVec;

#[test]
fn record_chunks_bound_memory_and_close_each_redb_read_window() {
    let directory = tempfile::tempdir().unwrap();
    let owner = crate::RedbStorage::open(directory.path().join("chunks.redb")).unwrap();
    let store = owner.record_store().unwrap();
    let setup = StorageReadControl::with_limit(32 << 20);
    let value: Vec<u8> = (0..(1 << 20) + 17).map(|i| (i % 251) as u8).collect();
    let transaction = store.allocate_transaction(&setup).unwrap();
    let sequence = store
        .commit(
            transaction,
            &PreparedRecordCommit::new(
                &[RecordWrite {
                    key: b"large",
                    expected: None,
                    value: Some(&value),
                }],
                &setup,
            )
            .unwrap(),
            &setup,
        )
        .unwrap()
        .sequence;
    let control = StorageReadControl::with_limit(2048);
    let cut = store.capture_backup_snapshot(&control).unwrap().unwrap();
    assert!(matches!(
        cut.records().get(b"large", &control),
        Err(VersionError::Memory(_))
    ));
    let mut output = BudgetedVec::new(control.memory());
    output.extend_from_slice(&[0; 1024]).unwrap();
    let mut offset = 0;
    while offset < value.len() {
        let before = crate::mvcc::read::READ_WINDOWS.get();
        let chunk = cut
            .records()
            .read_value_chunk(b"large", offset, &mut output, &control)
            .unwrap()
            .unwrap();
        assert_eq!(crate::mvcc::read::READ_WINDOWS.get(), before + 1);
        assert_eq!(chunk.revision(), sequence);
        assert_eq!(chunk.total_bytes(), Some(value.len()));
        assert_eq!(
            &output[..chunk.copied()],
            &value[offset..offset + chunk.copied()]
        );
        if offset == 0 {
            let writer = store.clone();
            let thread = std::thread::spawn(move || {
                let setup = StorageReadControl::with_limit(1 << 20);
                let transaction = writer.allocate_transaction(&setup).unwrap();
                writer
                    .commit(
                        transaction,
                        &PreparedRecordCommit::new(
                            &[RecordWrite {
                                key: b"large",
                                expected: Some(sequence),
                                value: Some(b"later"),
                            }],
                            &setup,
                        )
                        .unwrap(),
                        &setup,
                    )
                    .unwrap();
                writer.reclaim_versions(&setup).unwrap();
            });
            thread.join().unwrap();
        }
        offset += chunk.copied();
    }
    assert_eq!(
        cut.records()
            .read_value_chunk(b"large", offset, &mut output, &control)
            .unwrap()
            .unwrap()
            .copied(),
        0
    );
    assert!(control.memory().peak() <= 2048);
    drop(cut);
    drop(output);
    assert_eq!(control.memory().used(), 0);
    assert_eq!(
        &***store
            .snapshot(&setup)
            .unwrap()
            .get(b"large", &setup)
            .unwrap()
            .unwrap()
            .value()
            .unwrap(),
        b"later"
    );
}
