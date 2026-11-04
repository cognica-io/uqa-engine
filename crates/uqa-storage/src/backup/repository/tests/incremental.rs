//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::image_fixture::{capture, objects, read, Image, Unit};
use super::*;
use std::sync::atomic::Ordering;

#[test]
fn incremental_capture_reuses_exact_images_and_preserves_complete_current_membership() {
    for protection in [Protection::Plaintext, protected()] {
        let directory = tempfile::tempdir().unwrap();
        let control = control();
        let repository = LocalRepository::create(
            directory.path().join("backup"),
            protection,
            limits(),
            &control,
        )
        .unwrap();
        let mut previous = vec![
            Unit::record(0, b""),
            Unit::record(1, b"unchanged"),
            Unit::record(2, b"old!"),
            Unit::record(3, b"deleted"),
            Unit::record(4, b""),
            Unit::record(5, b"gone"),
            Unit::record(6, b"moved"),
            Unit::record(9, b"same suffix"),
        ];
        previous[4].kind = UnitKind::Tombstone;
        let mut incarnation = Unit::record(7, b"incarnation");
        incarnation.generation = 1;
        previous.push(incarnation.clone());
        let (parent, full_report) =
            capture(&repository, None, previous.clone(), &control, 16 << 20);
        assert_eq!(full_report.image.written_units, previous.len() as u64);
        assert_eq!(full_report.image.reused_units, 0);
        let original = read(&repository, parent, &control);
        let before = objects(&repository);
        let mut current = vec![
            Unit::record(0, b""),
            Unit::record(1, b"unchanged"),
            Unit::record(2, b"new!"),
            Unit::record(4, b""),
            Unit::record(5, b""),
            Unit::record(8, b"inserted"),
            Unit::record(9, b"same suffix"),
        ];
        current[4].kind = UnitKind::Tombstone;
        incarnation.generation = 2;
        current.push(incarnation);
        let mut moved = Unit::record(6, b"moved");
        moved.namespace = 8;
        current.push(moved);
        let (child, report) = capture(
            &repository,
            Some(parent.backup()),
            current.clone(),
            &control,
            16 << 20,
        );
        let after = objects(&repository);
        assert_eq!(after.1 - before.1, report.image.written_object_bytes);
        assert_eq!(report.image.parent_units, previous.len() as u64);
        assert_eq!(report.image.reused_units, 3);
        assert_eq!(report.image.reused_payload_bytes, 20);
        assert_eq!(report.image.written_units, 6);
        assert_eq!(
            report.image.hashed_payload_bytes,
            report.inventory.decoded_bytes
        );
        assert_eq!(
            report.image.written_payload_bytes + report.image.reused_payload_bytes,
            report.inventory.decoded_bytes
        );
        assert_eq!(
            report.source.copied_payload_bytes,
            report.inventory.decoded_bytes
        );
        let restored = read(&repository, child, &control);
        assert_eq!(
            restored.iter().map(|(unit, _)| unit).collect::<Vec<_>>(),
            current.iter().collect::<Vec<_>>()
        );
        for (unit, span) in &restored {
            if let Some((_, old_span)) = original.iter().find(|(old, _)| old == unit) {
                assert_eq!(span, old_span);
            } else {
                assert!(original
                    .iter()
                    .all(|(_, old_span)| span.object() != old_span.object()));
            }
        }
        let (independent, _) = capture(&repository, None, current, &control, 16 << 20);
        assert_eq!(independent.summary(), child.summary());
        assert_eq!(control.memory().used(), 0);
    }
}

#[test]
fn incremental_children_and_branches_survive_parent_manifest_removal() {
    let directory = tempfile::tempdir().unwrap();
    let control = control();
    let repository = LocalRepository::create(
        directory.path().join("backup"),
        protected(),
        limits(),
        &control,
    )
    .unwrap();
    let records = vec![Unit::record(1, b"one"), Unit::record(2, b"two")];
    let (parent, _) = capture(&repository, None, records.clone(), &control, 1 << 20);
    let before = objects(&repository);
    let (same, report) = capture(
        &repository,
        Some(parent.backup()),
        records.clone(),
        &control,
        1 << 20,
    );
    assert_eq!(report.image.written_units, 0);
    assert_eq!(report.image.written_payload_bytes, 0);
    assert_eq!(
        objects(&repository).0 - before.0,
        1,
        "only the child's inventory is a new object"
    );
    let (branch, _) = capture(
        &repository,
        Some(parent.backup()),
        vec![records[1].clone()],
        &control,
        1 << 20,
    );
    let (empty, report) = capture(
        &repository,
        Some(same.backup()),
        Vec::new(),
        &control,
        1 << 20,
    );
    assert_eq!(report.inventory.units, 0);
    assert_eq!(report.image.parent_units, 2);
    let name = super::super::directory::name(parent.backup().as_bytes());
    repository
        .roots
        .remove_file(super::super::directory::text(&name))
        .unwrap();
    assert_eq!(
        read(&repository, same, &control)
            .into_iter()
            .map(|(unit, _)| unit)
            .collect::<Vec<_>>(),
        records
    );
    assert_eq!(read(&repository, branch, &control)[0].0.data, b"two");
    assert!(read(&repository, empty, &control).is_empty());
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn incremental_fallback_streams_large_equal_and_changed_units_with_bounded_memory() {
    let directory = tempfile::tempdir().unwrap();
    let control = StorageReadControl::with_limit(128 << 10);
    let repository = LocalRepository::create(
        directory.path().join("backup"),
        protected(),
        limits(),
        &control,
    )
    .unwrap();
    let mut records = (0..257)
        .map(|id| Unit::record(id, b"small"))
        .collect::<Vec<_>>();
    records[128].data = vec![42; (2 << 20) + 7];
    let (parent, _) = capture(&repository, None, records.clone(), &control, 16 << 20);
    let before = objects(&repository);
    let (same, report) = capture(
        &repository,
        Some(parent.backup()),
        records.clone(),
        &control,
        16 << 20,
    );
    assert_eq!(objects(&repository).0 - before.0, 1);
    assert_eq!(report.image.reused_units, 257);
    assert_eq!(
        report.source.copied_payload_bytes,
        report.inventory.decoded_bytes
    );
    assert_eq!(
        report.image.hashed_payload_bytes,
        report.inventory.decoded_bytes
    );
    records[128].data[1 << 20] = 99;
    let (changed, report) = capture(
        &repository,
        Some(same.backup()),
        records.clone(),
        &control,
        16 << 20,
    );
    assert_eq!(report.image.written_units, 1);
    assert_eq!(report.image.reused_units, 256);
    assert_eq!(report.image.written_payload_bytes, (2 << 20) + 7);
    assert_eq!(
        read(&repository, changed, &control)[128].0.data,
        records[128].data
    );
    assert!(control.memory().peak() <= 128 << 10);
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn incremental_capture_rejects_corrupt_parent_metadata_and_reused_objects_before_publication() {
    use std::io::{Read, Seek, SeekFrom, Write};
    for corrupt_inventory in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let control = control();
        let repository = LocalRepository::create(
            directory.path().join("backup"),
            protected(),
            limits(),
            &control,
        )
        .unwrap();
        let records = vec![Unit::record(1, b"retained")];
        let (parent, _) = capture(&repository, None, records.clone(), &control, 1 << 20);
        let object = if corrupt_inventory {
            parent.inventory().object()
        } else {
            read(&repository, parent, &control)[0].1.object()
        };
        let name = super::super::directory::name(object.as_bytes());
        let mut file = super::super::directory::open_file(
            &repository.objects,
            super::super::directory::text(&name),
            true,
        )
        .unwrap();
        let mut byte = [0];
        file.read_exact(&mut byte).unwrap();
        byte[0] ^= 1;
        file.seek(SeekFrom::Start(0)).unwrap();
        file.write_all(&byte).unwrap();
        drop(file);
        let source = Image::new(records);
        let dropped = source.dropped.clone();
        let mut operation = repository
            .begin_backup(source.cut(), Some(parent.backup()), 1 << 20, &control)
            .unwrap();
        assert!(operation.prepare_image(source, 1 << 20, &control).is_err());
        assert!(dropped.load(Ordering::SeqCst));
        assert!(matches!(
            operation.publish(&control),
            Err(BackupError::Finished)
        ));
        assert_eq!(operation.cancel().unwrap(), None);
        assert_eq!(control.memory().used(), 0);
    }
}

#[test]
fn incremental_capture_denial_cancellation_and_truncated_sources_release_every_partial_owner() {
    for fault in 0..3 {
        let directory = tempfile::tempdir().unwrap();
        let control = control();
        let repository = LocalRepository::create(
            directory.path().join("backup"),
            protected(),
            limits(),
            &control,
        )
        .unwrap();
        let records = vec![Unit::record(1, &vec![7; 1 << 20])];
        let (parent, _) = capture(&repository, None, records.clone(), &control, 4 << 20);
        let mut source = Image::new(if fault == 2 {
            vec![Unit::record(1, &vec![7; (1 << 20) - 1])]
        } else {
            records
        });
        if fault == 1 {
            source.cancel = Some(control.cancellation().clone());
        }
        if fault == 2 {
            source.length_delta = 1;
        }
        let dropped = source.dropped.clone();
        let mut operation = repository
            .begin_backup(source.cut(), Some(parent.backup()), 8 << 20, &control)
            .unwrap();
        let error = operation
            .prepare_image(
                source,
                if fault == 0 { 64 << 10 } else { 4 << 20 },
                &control,
            )
            .unwrap_err();
        match fault {
            0 => assert!(matches!(
                error,
                BackupError::Limit {
                    resource: "capture workspace",
                    ..
                }
            )),
            1 => assert!(matches!(error, BackupError::Cancelled(_))),
            _ => assert!(matches!(error, BackupError::Integrity)),
        }
        assert!(dropped.load(Ordering::SeqCst));
        assert!(matches!(
            operation.publish(&control),
            Err(BackupError::Finished)
        ));
        assert_eq!(operation.cancel().unwrap(), None);
        assert_eq!(control.memory().used(), 0);
    }
}
