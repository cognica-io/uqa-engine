//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::mvcc::{
    CommitSequence, CommittedRecordSnapshot, RecordMetadata, RecordVersion, ScannedRecord,
};
use std::sync::atomic::{AtomicUsize, Ordering};

fn records(control: &StorageReadControl) -> Records {
    Records::new(&control.memory().child(control.memory().limit() / 32))
}

#[test]
fn spilled_edits_share_the_resident_prefix_without_reloading_its_payload() {
    let control = StorageReadControl::with_limit(32 * 1024);
    let mut records = records(&control);
    records
        .push(
            b"resident",
            Some(b"shared"),
            RecordWriteKind::Canonical,
            false,
            &control,
        )
        .unwrap();
    let original = records.resident[0].value.as_ref().unwrap().clone();
    let payload = vec![7; 4096];
    records
        .push(
            b"spilled",
            Some(&payload),
            RecordWriteKind::Canonical,
            false,
            &control,
        )
        .unwrap();
    let mut count = 0;
    records
        .visit(&control, |edit| {
            if count == 0 {
                assert_eq!(edit.key.bytes(), b"resident");
                assert!(Arc::ptr_eq(edit.value.as_ref().unwrap(), &original));
            } else {
                assert_eq!(edit.key.bytes(), b"spilled");
                assert_eq!(&edit.value.as_ref().unwrap()[..], payload);
            }
            count += 1;
            Ok(())
        })
        .unwrap();
    assert_eq!(count, 2);
    drop((records, original));
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn record_groups_share_one_resident_allowance_and_spill_their_remainders() {
    let control = StorageReadControl::with_limit(32 * 1024);
    let memory = control.memory().child(1024);
    let payload = vec![5; 256];
    let mut groups = Vec::new();
    for index in 0..8_u64 {
        let mut records = Records::new(&memory);
        records
            .push(
                &index.to_le_bytes(),
                Some(&payload),
                RecordWriteKind::Canonical,
                false,
                &control,
            )
            .unwrap();
        groups.push(records);
    }
    assert!(memory.used() > 0 && memory.used() <= memory.limit());
    for (index, records) in groups.iter().enumerate() {
        let mut count = 0;
        records
            .visit(&control, |edit| {
                assert_eq!(edit.key.bytes(), &(index as u64).to_le_bytes());
                assert_eq!(&edit.value.as_ref().unwrap()[..], payload);
                count += 1;
                Ok(())
            })
            .unwrap();
        assert_eq!(count, 1);
    }
    drop(groups);
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn evaluated_record_spill_preserves_order_kinds_empty_values_and_prefixes() {
    let control = StorageReadControl::with_limit(32 * 1024);
    let mut records = records(&control);
    let payload = vec![0xA5; 4096];
    records
        .push(
            b"empty",
            Some(b""),
            RecordWriteKind::Canonical,
            false,
            &control,
        )
        .unwrap();
    for index in 0..20_u64 {
        records
            .push(
                &index.to_le_bytes(),
                Some(&payload),
                RecordWriteKind::HNSWPreview,
                false,
                &control,
            )
            .unwrap();
        records
            .push(
                b"edges/",
                None,
                RecordWriteKind::HNSWPreview,
                true,
                &control,
            )
            .unwrap();
    }
    records
        .push(
            b"deleted",
            None,
            RecordWriteKind::Canonical,
            false,
            &control,
        )
        .unwrap();
    let path = records
        .spilled
        .as_ref()
        .unwrap()
        .file()
        .path()
        .to_path_buf();
    let disk = std::fs::read(&path).unwrap();
    assert!(!disk.windows(payload.len()).any(|bytes| bytes == payload));
    let mut position = 0;
    records
        .visit(&control, |edit| {
            if position == 0 {
                assert_eq!(edit.key.bytes(), b"empty");
                assert!(edit.value.as_ref().unwrap().is_empty());
            } else if position == 41 {
                assert_eq!(edit.key.bytes(), b"deleted");
                assert!(edit.value.is_none());
                assert!(!edit.prefix);
            } else if position % 2 == 1 {
                assert_eq!(edit.key.bytes(), &((position - 1) as u64 / 2).to_le_bytes());
                assert_eq!(&edit.value.as_ref().unwrap()[..], payload);
                assert!(edit.kind == RecordWriteKind::HNSWPreview);
                assert!(!edit.prefix);
            } else {
                assert_eq!(edit.key.bytes(), b"edges/");
                assert!(edit.value.is_none() && edit.prefix);
            }
            position += 1;
            Ok(())
        })
        .unwrap();
    assert_eq!(position, 42);
    assert!(control.memory().peak() <= control.memory().limit());
    drop(records);
    assert_eq!(control.memory().used(), 0);
    assert!(!path.exists());
}

#[test]
fn evaluated_record_spill_rejects_failed_appends_without_losing_prior_edits() {
    let control = StorageReadControl::with_limit(32 * 1024);
    let mut records = records(&control);
    let payload = vec![7; 4096];
    records
        .push(
            b"kept",
            Some(&payload),
            RecordWriteKind::Canonical,
            false,
            &control,
        )
        .unwrap();
    records
        .spilled
        .as_ref()
        .unwrap()
        .file()
        .fail_write_after(13);
    assert!(records
        .push(
            b"failed",
            Some(&payload),
            RecordWriteKind::Canonical,
            false,
            &control
        )
        .is_err());
    records
        .spilled
        .as_ref()
        .unwrap()
        .file()
        .fail_write_after(usize::MAX);
    control.cancellation().cancel();
    assert!(records
        .push(
            b"cancelled",
            None,
            RecordWriteKind::Canonical,
            false,
            &control
        )
        .is_err());
    assert!(records.visit(&control, |_| Ok(())).is_err());
    control.cancellation().reset();
    records
        .push(b"last", None, RecordWriteKind::Canonical, false, &control)
        .unwrap();
    let mut keys = Vec::new();
    records
        .visit(&control, |edit| {
            keys.push(edit.key.bytes().to_vec());
            Ok(())
        })
        .unwrap();
    assert_eq!(keys, [b"kept".to_vec(), b"last".to_vec()]);
    assert!(records
        .visit(&control, |_| Err(VersionError::InvalidEncoding(
            "consumer failure"
        )))
        .is_err());
    let mut again = 0;
    records
        .visit(&control, |_| {
            again += 1;
            Ok(())
        })
        .unwrap();
    assert_eq!(again, 2);
    drop(records);
    assert_eq!(control.memory().used(), 0);
}

struct FailingMetadata {
    reads: Arc<AtomicUsize>,
}

impl CommittedRecordSnapshot for FailingMetadata {
    fn sequence(&self) -> CommitSequence {
        CommitSequence::from_u64(0)
    }

    fn metadata(
        &self,
        key: &[u8],
        control: &StorageReadControl,
    ) -> VersionResult<Option<RecordMetadata>> {
        control.check()?;
        self.reads.fetch_add(1, Ordering::Relaxed);
        if key == b"bad" {
            return Err(VersionError::InvalidEncoding("rejected record metadata"));
        }
        Ok(None)
    }

    fn get(
        &self,
        _: &[u8],
        _: &StorageReadControl,
    ) -> VersionResult<Option<RecordVersion<SharedRecordValue>>> {
        panic!("metadata application must not fetch committed values")
    }

    fn scan(
        &self,
        _: &[u8],
        _: Option<&[u8]>,
        _: usize,
        _: &StorageReadControl,
    ) -> VersionResult<BudgetedVec<ScannedRecord>> {
        panic!("point application must not enumerate committed records")
    }
}

#[test]
fn failed_record_metadata_stream_restores_private_root_before_later_edits() {
    let control = StorageReadControl::with_limit(128 << 10);
    let reads = Arc::new(AtomicUsize::new(0));
    let mut transaction = super::super::Transaction::at_snapshot(
        Arc::new(FailingMetadata {
            reads: Arc::clone(&reads),
        }),
        false,
        &control,
    );
    transaction
        .replace(b"kept", Some(b"original"), &control)
        .unwrap();
    let retained = transaction.view().unwrap();
    let mut edits = records(&control);
    for id in 0..64_u64 {
        edits
            .push(
                &id.to_be_bytes(),
                Some(&[id as u8; 512]),
                RecordWriteKind::Canonical,
                false,
                &control,
            )
            .unwrap();
    }
    edits
        .push(
            b"bad",
            Some(b"fail"),
            RecordWriteKind::Canonical,
            false,
            &control,
        )
        .unwrap();
    edits
        .push(
            b"later",
            Some(b"unread"),
            RecordWriteKind::Canonical,
            false,
            &control,
        )
        .unwrap();
    assert!(edits.spilled.is_some());
    reads.store(0, Ordering::Relaxed);
    let error = transaction
        .atomic(|transaction| edits.apply(transaction, &control))
        .unwrap_err();
    assert!(matches!(
        error,
        VersionError::InvalidEncoding("rejected record metadata")
    ));
    assert_eq!(reads.load(Ordering::Relaxed), 65);
    for view in [retained, transaction.view().unwrap()] {
        view.visit_value(b"kept", &control, &mut |record| {
            assert_eq!(record.unwrap().value, Some(&b"original"[..]));
            Ok(())
        })
        .unwrap();
        for id in 0..64_u64 {
            assert!(view
                .metadata(&id.to_be_bytes(), &control)
                .unwrap()
                .is_none());
        }
        assert!(view.metadata(b"later", &control).unwrap().is_none());
    }
    drop((transaction, edits));
    assert_eq!(control.memory().used(), 0);
    assert!(control.memory().peak() <= control.memory().limit());
}
