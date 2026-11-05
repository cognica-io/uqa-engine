//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::image_fixture::{capture, read, Image, Unit};
use super::*;
use std::sync::atomic::Ordering;

mod failures;

fn repository(path: &std::path::Path, control: &StorageReadControl) -> LocalRepository {
    LocalRepository::create(path.join("repository"), protected(), limits(), control).unwrap()
}

fn finish<S: BackupImageSource>(job: &mut BackupImageCapture<'_, S>, bytes: usize) {
    let mut calls = 0;
    loop {
        let before = job.progress();
        let step = job.advance(1, bytes).unwrap();
        let after = job.progress();
        assert!(after.source.visited_positions - before.source.visited_positions <= 1);
        assert!(
            after.source.copied_payload_bytes - before.source.copied_payload_bytes <= bytes as u64
        );
        assert!(
            after.image.hashed_payload_bytes - before.image.hashed_payload_bytes <= bytes as u64
        );
        assert!(after.image.parent_units - before.image.parent_units <= 1);
        assert!(after.verified_object_bytes - before.verified_object_bytes <= bytes as u64);
        calls += 1;
        assert!(
            calls < 500_000,
            "finite image must finish under a positive window"
        );
        if step == BackupCaptureStep::Prepared {
            assert_eq!(after.phase, BackupCapturePhase::Prepared);
            assert!(job.report().is_some());
            assert!(job.completed().is_none());
            break;
        }
        assert_eq!(step, BackupCaptureStep::Pending);
        assert!(job.report().is_none());
        assert!(matches!(job.publish(), Err(BackupError::Finished)));
    }
}

#[test]
fn scheduled_capture_bounds_full_and_incremental_work_and_keeps_exact_images() {
    let directory = tempfile::tempdir().unwrap();
    let control = StorageReadControl::with_limit(128 << 10);
    let repository = repository(directory.path(), &control);
    let mut units = (0..257)
        .map(|key| Unit::record(key, b"unchanged"))
        .collect::<Vec<_>>();
    units[128].data = vec![42; (64 << 10) + 7];
    units[0].data.clear();
    let source = Image::new(units.clone());
    let dropped = source.dropped.clone();
    let cut = source.cut();
    let mut job = repository
        .begin_image_backup(source, None, 16 << 20, 2 << 20, &control)
        .unwrap();
    assert_eq!(job.cut(), cut);
    assert!(!dropped.load(Ordering::SeqCst));
    finish(&mut job, 257);
    assert!(dropped.load(Ordering::SeqCst));
    let report = job.report().unwrap();
    assert_eq!(report.inventory.units, units.len() as u64);
    assert_eq!(
        report.image.hashed_payload_bytes,
        report.inventory.decoded_bytes
    );
    assert_eq!(
        report.source.copied_payload_bytes,
        report.inventory.decoded_bytes
    );
    let parent = job.publish().unwrap();
    assert_eq!(job.completed(), Some(parent));
    assert_eq!(job.advance(0, 0).unwrap(), BackupCaptureStep::Complete);
    assert_eq!(job.cancel().unwrap(), Some(parent));
    assert_eq!(job.publish().unwrap(), parent);
    drop(job);
    assert_eq!(control.memory().used(), 0);
    let original = read(&repository, parent, &control);
    let mut current = vec![units[100].clone(), units[128].clone(), units[200].clone()];
    current[1].data[999] = 7;
    let mut job = repository
        .begin_image_backup(
            Image::new(current.clone()),
            Some(parent.backup()),
            16 << 20,
            2 << 20,
            &control,
        )
        .unwrap();
    finish(&mut job, 127);
    let report = job.report().unwrap();
    assert_eq!(report.image.parent_units, 257);
    assert_eq!(report.image.reused_units, 2);
    assert_eq!(report.image.written_units, 1);
    assert_eq!(
        report.image.hashed_payload_bytes,
        report.inventory.decoded_bytes
    );
    assert_eq!(
        report.source.copied_payload_bytes,
        report.inventory.decoded_bytes
    );
    let child = job.publish().unwrap();
    drop(job);
    repository.remove_backup(parent.backup(), &control).unwrap();
    let restored = read(&repository, child, &control);
    assert_eq!(
        restored.iter().map(|(unit, _)| unit).collect::<Vec<_>>(),
        current.iter().collect::<Vec<_>>()
    );
    assert_eq!(restored[0].1, original[100].1);
    assert_ne!(restored[1].1.object(), original[128].1.object());
    assert_eq!(restored[2].1, original[200].1);
    let (independent, _) = capture(&repository, None, current, &control, 2 << 20);
    assert_eq!(independent.summary(), child.summary());
    assert_eq!(control.memory().used(), 0);
    assert!(control.memory().peak() <= 128 << 10);
}

#[test]
fn scheduled_capture_handles_empty_images_and_single_byte_windows() {
    for units in [
        vec![],
        vec![Unit::record(1, b""), Unit::record(2, b"123456789")],
    ] {
        let directory = tempfile::tempdir().unwrap();
        let control = control();
        let repository = repository(directory.path(), &control);
        let mut job = repository
            .begin_image_backup(Image::new(units.clone()), None, 1 << 20, 1 << 20, &control)
            .unwrap();
        finish(&mut job, 1);
        let root = job.publish().unwrap();
        drop(job);
        assert_eq!(
            read(&repository, root, &control)
                .into_iter()
                .map(|(unit, _)| unit)
                .collect::<Vec<_>>(),
            units
        );
        assert_eq!(control.memory().used(), 0);
    }
}

#[test]
fn scheduled_capture_cancels_at_capture_verification_and_prepared_boundaries() {
    for target in [
        BackupCapturePhase::Capturing,
        BackupCapturePhase::Verifying,
        BackupCapturePhase::Prepared,
    ] {
        let directory = tempfile::tempdir().unwrap();
        let control = control();
        let repository = repository(directory.path(), &control);
        let source = Image::new(vec![Unit::record(1, &[42; 8192])]);
        let dropped = source.dropped.clone();
        let mut job = repository
            .begin_image_backup(source, None, 1 << 20, 1 << 20, &control)
            .unwrap();
        let id = job.id();
        while job.progress().phase != target || job.progress().source.copied_payload_bytes == 0 {
            job.advance(1, 127).unwrap();
        }
        control.cancellation().cancel();
        if target == BackupCapturePhase::Prepared {
            assert!(matches!(job.publish(), Err(BackupError::Cancelled(_))));
        } else {
            assert!(matches!(
                job.advance(1, 127),
                Err(BackupError::Cancelled(_))
            ));
            assert_eq!(job.progress().phase, BackupCapturePhase::Failed);
        }
        assert!(dropped.load(Ordering::SeqCst));
        assert_eq!(job.cancel().unwrap(), None);
        assert_eq!(job.progress().phase, BackupCapturePhase::Cancelled);
        assert_eq!(job.cancel().unwrap(), None);
        assert!(matches!(job.publish(), Err(BackupError::Finished)));
        assert!(matches!(job.advance(1, 127), Err(BackupError::Finished)));
        drop(job);
        assert_eq!(control.memory().used(), 0);
        let independent = super::control();
        assert!(repository.open_backup(id, &independent).is_err());
        assert!(matches!(
            repository.resume_publication(id, 1 << 20, &independent),
            Err(BackupError::Finished)
        ));
    }
}

#[test]
fn scheduled_capture_resolves_uncertain_publication_before_original_cancellation() {
    let directory = tempfile::tempdir().unwrap();
    let control = control();
    let repository = repository(directory.path(), &control);
    let mut job = repository
        .begin_image_backup(
            Image::new(vec![Unit::record(1, b"durable")]),
            None,
            1 << 20,
            1 << 20,
            &control,
        )
        .unwrap();
    finish(&mut job, 128);
    *repository.fault.lock() = Some(Fault::RootSync);
    assert!(job.publish().is_err());
    assert_eq!(job.progress().phase, BackupCapturePhase::Prepared);
    control.cancellation().cancel();
    let root = job.publish().unwrap();
    assert_eq!(job.cancel().unwrap(), Some(root));
    assert_eq!(job.completed(), Some(root));
    assert_eq!(job.progress().phase, BackupCapturePhase::Complete);
    drop(job);
    assert_eq!(control.memory().used(), 0);
    repository
        .open_backup(root.backup(), &super::control())
        .unwrap()
        .verify(1 << 20, &super::control())
        .unwrap();
}

#[test]
fn scheduled_capture_rechecks_repository_generation_after_incremental_verification() {
    let directory = tempfile::tempdir().unwrap();
    let control = control();
    let repository = repository(directory.path(), &control);
    let mut job = repository
        .begin_image_backup(
            Image::new(vec![Unit::record(1, b"retained")]),
            None,
            1 << 20,
            1 << 20,
            &control,
        )
        .unwrap();
    let id = job.id();
    while job.progress().phase != BackupCapturePhase::Verifying {
        job.advance(1, 128).unwrap();
    }
    repository
        .begin_backup(cut(), None, 1 << 20, &control)
        .unwrap()
        .cancel()
        .unwrap();
    loop {
        match job.advance(1, 128) {
            Ok(BackupCaptureStep::Pending) => (),
            Err(BackupError::Busy) => break,
            result => panic!("changed repository cannot acquire prepared authority: {result:?}"),
        }
    }
    assert_eq!(job.progress().phase, BackupCapturePhase::Failed);
    assert!(matches!(job.publish(), Err(BackupError::Finished)));
    assert_eq!(job.cancel().unwrap(), None);
    drop(job);
    assert_eq!(control.memory().used(), 0);
    assert!(repository.open_backup(id, &control).is_err());
}
