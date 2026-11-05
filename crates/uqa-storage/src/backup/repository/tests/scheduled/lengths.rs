//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

fn streaming(units: Vec<Unit>) -> Image {
    let mut image = Image::new(units);
    image.streaming_length = true;
    image
}

fn publish(
    repository: &LocalRepository,
    image: Image,
    parent: Option<BackupId>,
    control: &StorageReadControl,
) -> (BackupRoot, BackupCaptureReport) {
    let mut job = repository
        .begin_image_backup(image, parent, 16 << 20, 2 << 20, control)
        .unwrap();
    finish(&mut job, 127);
    let report = job.report().unwrap();
    assert_eq!(
        report.source.copied_payload_bytes,
        report.inventory.decoded_bytes
    );
    assert_eq!(
        report.image.hashed_payload_bytes,
        report.inventory.decoded_bytes
    );
    (job.publish().unwrap(), report)
}

#[test]
fn streaming_lengths_preserve_full_and_incremental_inventory_and_reference_equality() {
    for parent_streams in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let control = StorageReadControl::with_limit(128 << 10);
        let repository = repository(directory.path(), &control);
        let original = vec![
            Unit::record(1, b""),
            Unit::record(2, &[7; 65_543]),
            Unit::record(3, b"equal-size"),
            Unit::record(4, b"shrinks"),
            Unit::record(5, b"grows"),
            Unit::record(6, b"removed"),
        ];
        let mut source = Image::new(original.clone());
        source.streaming_length = parent_streams;
        let (parent, _) = publish(&repository, source, None, &control);
        let old = read(&repository, parent, &control);
        let current = vec![
            original[0].clone(),
            original[1].clone(),
            Unit::record(3, b"changed!!!"),
            Unit::record(4, b""),
            Unit::record(5, &[9; 65_537]),
            Unit::record(7, b"new"),
        ];
        let (child, report) = publish(
            &repository,
            streaming(current.clone()),
            Some(parent.backup()),
            &control,
        );
        assert_eq!(report.image.parent_units, 6);
        assert_eq!(report.image.reused_units, 2);
        assert_eq!(report.image.written_units, 4);
        assert_eq!(report.image.reused_payload_bytes, 65_543);
        let restored = read(&repository, child, &control);
        assert_eq!(
            restored.iter().map(|entry| &entry.0).collect::<Vec<_>>(),
            current.iter().collect::<Vec<_>>()
        );
        assert_eq!(restored[0].1, old[0].1);
        assert_eq!(restored[1].1, old[1].1);
        for index in 2..5 {
            assert_ne!(restored[index].1, old[index].1);
        }
        let (known, _) = publish(&repository, Image::new(current.clone()), None, &control);
        let (unknown, _) = publish(&repository, streaming(current.clone()), None, &control);
        assert_eq!(known.summary(), child.summary());
        assert_eq!(unknown.summary(), child.summary());
        let (grandchild, report) = publish(
            &repository,
            Image::new(current),
            Some(child.backup()),
            &control,
        );
        assert_eq!(report.image.reused_units, 6);
        assert_eq!(report.image.written_units, 0);
        assert_eq!(grandchild.summary(), child.summary());
        repository.remove_backup(parent.backup(), &control).unwrap();
        repository.remove_backup(child.backup(), &control).unwrap();
        assert_eq!(read(&repository, grandchild, &control), restored);
        assert_eq!(control.memory().used(), 0);
        assert!(control.memory().peak() <= 128 << 10);
    }
}

#[test]
fn streaming_lengths_include_empty_units_and_single_byte_reads() {
    let directory = tempfile::tempdir().unwrap();
    let control = control();
    let repository = repository(directory.path(), &control);
    let units = vec![Unit::record(1, b""), Unit::record(2, b"abc")];
    let mut job = repository
        .begin_image_backup(streaming(units.clone()), None, 1 << 20, 1 << 20, &control)
        .unwrap();
    finish(&mut job, 1);
    let report = job.report().unwrap();
    assert_eq!(report.inventory.units, 2);
    assert_eq!(report.inventory.decoded_bytes, 3);
    assert_eq!(report.image.written_payload_bytes, 3);
    let root = job.publish().unwrap();
    drop(job);
    assert_eq!(
        read(&repository, root, &control)
            .into_iter()
            .map(|entry| entry.0)
            .collect::<Vec<_>>(),
        units
    );
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn streaming_length_failure_and_cancel_release_the_source_without_publishing() {
    for fault in 0..3 {
        let directory = tempfile::tempdir().unwrap();
        let control = control();
        let repository = repository(directory.path(), &control);
        let original = vec![Unit::record(1, &[7; 16_385])];
        let (parent, _) = publish(&repository, Image::new(original.clone()), None, &control);
        let mut image = streaming(original);
        if fault == 0 {
            image.cancel = Some(control.cancellation().clone());
        }
        let dropped = image.dropped.clone();
        let mut job = repository
            .begin_image_backup(
                image,
                Some(parent.backup()),
                1 << 20,
                if fault == 1 { 48 << 10 } else { 1 << 20 },
                &control,
            )
            .unwrap();
        let id = job.id();
        loop {
            if let Err(error) = job.advance(1, 127) {
                assert!(matches!(
                    (fault, error),
                    (0, BackupError::Cancelled(_))
                        | (
                            1,
                            BackupError::Limit {
                                resource: "capture workspace",
                                ..
                            }
                        )
                ));
                break;
            }
            if fault == 2 && job.progress().source.copied_payload_bytes != 0 {
                assert!(job.cancel().unwrap().is_none());
                break;
            }
        }
        assert!(dropped.load(Ordering::SeqCst));
        assert!(matches!(job.advance(1, 127), Err(BackupError::Finished)));
        assert!(job.report().is_none());
        assert!(job.publish().is_err());
        drop(job);
        assert!(repository
            .open_backup(id, &StorageReadControl::with_limit(1 << 20))
            .is_err());
        assert_eq!(control.memory().used(), 0);
    }
}
