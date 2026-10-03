//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn evaluated_record_spill_preserves_order_kinds_empty_values_and_prefixes() {
    let control = StorageReadControl::with_limit(32 * 1024);
    let mut records = Records::new(&control);
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
    let mut records = Records::new(&control);
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
