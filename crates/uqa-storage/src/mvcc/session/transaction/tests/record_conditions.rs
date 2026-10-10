//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::mvcc::{
    overlay::run::{read_counts, SpilledRunWriter},
    PrivateRecordRevision,
};

fn spilled_transaction(count: u64, control: &StorageReadControl) -> Transaction {
    let mut transaction = transaction(control);
    let identity = PrivateRecordRevision::for_tests();
    let mut writer = SpilledRunWriter::new(count, count * 8, control.memory()).unwrap();
    for id in 0..count {
        writer
            .push(
                &id.to_be_bytes(),
                None,
                if id % 2 == 0 {
                    RecordWriteKind::Canonical
                } else {
                    RecordWriteKind::HNSWPreview
                },
                identity,
                (!id.is_multiple_of(3)).then_some(&[73; 1024][..]),
                control,
            )
            .unwrap();
    }
    transaction.changes =
        PrivateRecordChanges::from_spilled_run(writer.finish().unwrap(), identity, None, control)
            .unwrap();
    transaction
}

#[test]
fn spilled_record_conditions_read_one_entry_without_its_value() {
    for count in [128_u64, 512] {
        let control = StorageReadControl::with_limit(128 << 10);
        let transaction = spilled_transaction(count, &control);
        for id in [0, 1, 2, count - 1] {
            let key = id.to_be_bytes();
            read_counts::take();
            transaction
                .changes
                .write_metadata(&key, &control)
                .unwrap()
                .unwrap();
            let one_read = read_counts::take();
            assert!(one_read.entries > 0);
            assert_eq!(one_read.values, 0);
            for kind in [
                RecordWriteKind::Canonical,
                RecordWriteKind::GraphCache,
                RecordWriteKind::GraphPreview,
                RecordWriteKind::HNSWPreview,
                RecordWriteKind::IVFPreview,
                RecordWriteKind::DiskANNOrigin,
            ] {
                for deleted in [false, true] {
                    let actual = transaction
                        .record_condition(&key, deleted, kind, &control)
                        .unwrap();
                    let reads = read_counts::take();
                    assert_eq!(reads.entries, one_read.entries);
                    assert_eq!(reads.values, 0);
                    let skipped = deleted
                        && id.is_multiple_of(3)
                        && matches!(
                            kind,
                            RecordWriteKind::Canonical | RecordWriteKind::GraphPreview
                        );
                    let expected_kind = match kind {
                        RecordWriteKind::GraphPreview => {
                            if id % 2 == 0 {
                                RecordWriteKind::Canonical
                            } else {
                                RecordWriteKind::HNSWPreview
                            }
                        }
                        RecordWriteKind::HNSWPreview | RecordWriteKind::IVFPreview
                            if id % 2 == 0 =>
                        {
                            RecordWriteKind::Canonical
                        }
                        RecordWriteKind::DiskANNOrigin if id % 2 == 0 && !deleted => {
                            RecordWriteKind::Canonical
                        }
                        _ => kind,
                    };
                    assert!(actual == (!skipped).then_some((None, expected_kind)));
                }
            }
        }
        let missing = (count + 1).to_be_bytes();
        assert!(transaction
            .record_condition(&missing, true, RecordWriteKind::Canonical, &control)
            .unwrap()
            .is_none());
        control.cancellation().cancel();
        assert!(transaction
            .record_condition(
                &0_u64.to_be_bytes(),
                false,
                RecordWriteKind::HNSWPreview,
                &control
            )
            .is_err());
        drop(transaction);
        assert_eq!(control.memory().used(), 0);
        assert!(control.memory().peak() <= control.memory().limit());
    }
}

#[test]
fn batch_read_scope_reuses_blocks_and_releases_them_without_retaining_a_view() {
    let control = StorageReadControl::with_limit(128 << 10);
    let mut transaction = spilled_transaction(512, &control);
    let key = 1_u64.to_be_bytes();
    transaction.savepoint("before", &control).unwrap();
    let retained = transaction.view().unwrap();
    let baseline = control.memory().used();
    let readers = transaction.changes.read_scope(&control).unwrap();
    transaction.changes.write_metadata(&key, &control).unwrap();
    read_counts::take();
    for _ in 0..32 {
        assert!(
            transaction
                .record_condition(&key, false, RecordWriteKind::Canonical, &control)
                .unwrap()
                == Some((None, RecordWriteKind::Canonical))
        );
    }
    let reads = read_counts::take();
    assert!(reads.entries > 0);
    assert_eq!(reads.blocks, 0);
    assert_eq!(reads.values, 0);
    drop(readers);
    assert_eq!(control.memory().used(), baseline);
    transaction.changes.write_metadata(&key, &control).unwrap();
    assert!(read_counts::take().blocks > 0);

    let readers = transaction.changes.read_scope(&control).unwrap();
    transaction
        .replace(&key, Some(b"replacement"), &control)
        .unwrap();
    assert!(
        transaction
            .record_condition(&key, false, RecordWriteKind::HNSWPreview, &control)
            .unwrap()
            == Some((None, RecordWriteKind::Canonical))
    );
    retained
        .visit_value(&key, &control, &mut |record| {
            assert_eq!(record.unwrap().value, Some(&[73; 1024][..]));
            Ok(())
        })
        .unwrap();
    transaction.rollback_to("before", &control).unwrap();
    assert!(
        transaction
            .record_condition(&key, false, RecordWriteKind::HNSWPreview, &control)
            .unwrap()
            == Some((None, RecordWriteKind::HNSWPreview))
    );
    control.cancellation().cancel();
    assert!(transaction.changes.read_scope(&control).is_err());
    drop(readers);
    drop(retained);
    drop(transaction);
    assert_eq!(control.memory().used(), 0);
    assert!(control.memory().peak() <= control.memory().limit());
}
