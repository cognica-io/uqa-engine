//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

struct Source {
    image: Image,
    pending: usize,
    visits: u64,
    panic: bool,
}

impl BackupImageSource for Source {
    fn cut(&self) -> BackupCut {
        self.image.cut()
    }
    fn statistics(&self) -> BackupSourceStatistics {
        BackupSourceStatistics {
            visited_positions: self.image.statistics().visited_positions + self.visits,
            ..self.image.statistics()
        }
    }
    fn advance(&mut self, maximum: usize) -> BackupResult<BackupSourceStep> {
        if self.pending != 0 {
            let count = self.pending.min(maximum);
            self.pending -= count;
            self.visits += count as u64;
            return Ok(BackupSourceStep::Pending);
        }
        self.pending = 17;
        self.image.advance(maximum)
    }
    fn current_unit(&self) -> Option<BackupSourceUnit<'_>> {
        self.image.current_unit()
    }
    fn read_current(&mut self, output: &mut [u8]) -> BackupResult<usize> {
        assert!(!self.panic, "injected source panic");
        let length = output.len().min(1);
        self.image.read_current(&mut output[..length])
    }
}

#[test]
fn scheduled_capture_preserves_pending_positions_short_reads_and_dropped_sources() {
    let directory = tempfile::tempdir().unwrap();
    let control = control();
    let repository = repository(directory.path(), &control);
    let records = vec![Unit::record(1, b"one"), Unit::record(2, &[42; 4100])];
    let source = Source {
        image: Image::new(records.clone()),
        pending: 17,
        visits: 0,
        panic: false,
    };
    let dropped = source.image.dropped.clone();
    let mut job = repository
        .begin_image_backup(source, None, 1 << 20, 1 << 20, &control)
        .unwrap();
    finish(&mut job, 128);
    assert_eq!(job.report().unwrap().source.visited_positions, 54);
    assert!(dropped.load(Ordering::SeqCst));
    let root = job.publish().unwrap();
    drop(job);
    assert_eq!(
        read(&repository, root, &control)
            .into_iter()
            .map(|(unit, _)| unit)
            .collect::<Vec<_>>(),
        records
    );
    assert_eq!(control.memory().used(), 0);

    let source = Image::new(vec![Unit::record(3, &[42; 8192])]);
    let dropped = source.dropped.clone();
    let mut job = repository
        .begin_image_backup(source, None, 1 << 20, 1 << 20, &control)
        .unwrap();
    let id = job.id();
    while job.progress().source.copied_payload_bytes < 4096 {
        job.advance(1, 128).unwrap();
    }
    drop(job);
    assert!(dropped.load(Ordering::SeqCst));
    assert_eq!(control.memory().used(), 0);
    assert!(repository.open_backup(id, &control).is_err());
    assert!(repository
        .resume_publication(id, 1 << 20, &control)
        .is_err());
    let collection = repository.collect_garbage(32, 1 << 20, &control).unwrap();
    assert_eq!(collection.active_operations, 0);
    assert_eq!(collection.completed_backups, 1);
    assert!(collection.removed_staging_files > 0);
    assert_eq!(read(&repository, root, &control).len(), 2);
}

#[test]
fn scheduled_capture_unwind_retires_the_source_and_permanently_fails_the_job() {
    let directory = tempfile::tempdir().unwrap();
    let control = control();
    let repository = repository(directory.path(), &control);
    let source = Source {
        image: Image::new(vec![Unit::record(1, b"payload")]),
        pending: 0,
        visits: 0,
        panic: true,
    };
    let dropped = source.image.dropped.clone();
    let mut job = repository
        .begin_image_backup(source, None, 1 << 20, 1 << 20, &control)
        .unwrap();
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| loop {
        job.advance(1, 128).unwrap();
    }));
    assert!(result.is_err());
    assert!(dropped.load(Ordering::SeqCst));
    assert_eq!(job.progress().phase, BackupCapturePhase::Failed);
    assert!(matches!(job.advance(1, 128), Err(BackupError::Finished)));
    assert!(matches!(job.publish(), Err(BackupError::Finished)));
    assert_eq!(job.cancel().unwrap(), None);
    drop(job);
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn scheduled_capture_admission_and_step_failures_release_every_partial_owner() {
    for fault in 0..7 {
        let directory = tempfile::tempdir().unwrap();
        let control = control();
        let repository = repository(directory.path(), &control);
        let mut source = Image::new(vec![Unit::record(1, &[42; 8192])]);
        let dropped = source.dropped.clone();
        if fault == 0 {
            source.length_delta = 1;
        }
        let reserved = (fault == 1).then(|| {
            control
                .memory()
                .reserve(control.memory().available())
                .unwrap()
        });
        let staging = if fault == 2 { 1024 } else { 1 << 20 };
        let workspace = if fault == 3 { 1 } else { 1 << 20 };
        let result = repository.begin_image_backup(source, None, staging, workspace, &control);
        if fault == 1 || fault == 3 {
            let error = result
                .err()
                .expect("header and workspace admission must fail");
            if fault == 3 {
                assert!(matches!(
                    error,
                    BackupError::Limit {
                        resource: "capture workspace",
                        ..
                    }
                ));
            }
        } else {
            let mut job = result.unwrap();
            if fault == 4 {
                *repository.fault.lock() = Some(Fault::ObjectInstall);
            }
            if fault == 5 {
                *repository.fault.lock() = Some(Fault::PreparedSync);
            }
            let error = loop {
                match job.advance(usize::from(fault != 6), 128) {
                    Ok(BackupCaptureStep::Pending) => (),
                    Ok(other) => panic!("fault must not prepare an image: {other:?}"),
                    Err(error) => break error,
                }
            };
            if fault == 2 {
                assert!(matches!(
                    error,
                    BackupError::Limit {
                        resource: "encoded staging",
                        ..
                    }
                ));
            }
            assert_eq!(job.progress().phase, BackupCapturePhase::Failed);
            assert!(matches!(job.advance(1, 128), Err(BackupError::Finished)));
            assert!(matches!(job.publish(), Err(BackupError::Finished)));
            assert_eq!(job.cancel().unwrap(), None);
        }
        drop(reserved);
        assert!(dropped.load(Ordering::SeqCst));
        assert_eq!(control.memory().used(), 0);
        assert_eq!(repository.roots.entries().unwrap().count(), 0);
    }
}
