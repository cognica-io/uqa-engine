//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[derive(Clone, Default)]
struct Factory {
    imported: Arc<AtomicUsize>,
    recovered: Arc<AtomicUsize>,
    closed: Arc<AtomicUsize>,
    failure: usize,
}

struct Work<'a> {
    reader: Option<BackupImageReader<'a>>,
    file: File,
    closed: Arc<AtomicUsize>,
    failure: usize,
}

impl Drop for Work<'_> {
    fn drop(&mut self) {
        self.closed.fetch_add(1, Ordering::SeqCst);
    }
}

impl BackupRestoreWork for Work<'_> {
    fn advance(&mut self) -> BackupResult<BackupRestoreWorkStep> {
        if self.failure == 1 {
            return Err(BackupError::Integrity);
        }
        assert_ne!(self.failure, 2, "provider unwound during a step");
        if let Some(reader) = &mut self.reader {
            if reader.advance(7)? == BackupReadStep::Complete {
                self.reader = None;
            }
            return Ok(BackupRestoreWorkStep::Pending);
        }
        self.file.sync_all()?;
        Ok(BackupRestoreWorkStep::Complete)
    }
}

impl<'a> BackupRestoreFactory<'a> for Factory {
    type Work = RestoreFileWork<Work<'a>>;

    fn import(
        self,
        directory: &RestoreDirectory<'_>,
        backup: BackupLease<'a>,
        request: DatabaseRestore,
        control: &StorageReadControl,
    ) -> BackupResult<Self::Work> {
        self.imported.fetch_add(1, Ordering::SeqCst);
        let mut file = directory.create_file("database")?;
        file.write_all(&request.target().as_bytes())?;
        file.write_all(b"literal imported contents")?;
        drop(file);
        self.open(directory, backup, control)
    }

    fn recover(
        self,
        directory: &RestoreDirectory<'_>,
        backup: BackupLease<'a>,
        request: DatabaseRestore,
        control: &StorageReadControl,
    ) -> BackupResult<Self::Work> {
        self.recovered.fetch_add(1, Ordering::SeqCst);
        let mut file = directory.open_file("database", false)?;
        let mut identity = [0; 16];
        file.read_exact(&mut identity)?;
        if identity != request.target().as_bytes() {
            return Err(BackupError::Integrity);
        }
        drop(file);
        self.open(directory, backup, control)
    }
}

impl Factory {
    fn open<'a>(
        self,
        directory: &RestoreDirectory<'_>,
        backup: BackupLease<'a>,
        control: &StorageReadControl,
    ) -> BackupResult<RestoreFileWork<Work<'a>>> {
        if self.failure == 3 {
            return Err(BackupError::Invalid("fixture constructor failed"));
        }
        directory.begin_file_work("database", control, |path| {
            let file = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(path)?;
            file.try_lock().unwrap();
            Ok(Work {
                reader: Some(backup.into_image_reader(1 << 20, control)?),
                file,
                closed: self.closed,
                failure: self.failure,
            })
        })
    }
}

fn prepare<W: BackupRestoreWork>(job: &mut BackupRestoreJob<'_, W>) {
    for _ in 0..1000 {
        if job.advance().unwrap() == BackupRestoreStep::Prepared {
            return;
        }
    }
    panic!("restore did not prepare within its fixture work bound");
}

#[test]
fn scheduled_restore_owns_admission_and_closes_files_before_preparation() {
    let fixture = Fixture::new();
    let factory = Factory::default();
    let operation = fixture.begin();
    let retained = fixture.control.memory().used();
    let mut job = operation.into_job(factory.clone()).unwrap();
    let request = job.request();
    assert!(matches!(job.publish(), Err(BackupError::Finished)));
    let mut pending = 0;
    loop {
        assert!(!fixture.path().exists());
        assert!(matches!(
            fixture
                .repository
                .remove_backup(fixture.backup, &fixture.control),
            Err(BackupError::Busy)
        ));
        assert!(matches!(
            fixture
                .repository
                .resume_restore(job.id(), fixture.path(), &fixture.control),
            Err(BackupError::Busy)
        ));
        if job.advance().unwrap() == BackupRestoreStep::Prepared {
            break;
        }
        pending += 1;
        assert!(pending < 1000);
    }
    assert!(pending > 1);
    assert_eq!(factory.imported.load(Ordering::SeqCst), 1);
    assert_eq!(factory.closed.load(Ordering::SeqCst), 1);
    assert_eq!(fixture.control.memory().used(), retained);
    fixture
        .repository
        .remove_backup(fixture.backup, &fixture.control)
        .unwrap();
    assert_eq!(job.advance().unwrap(), BackupRestoreStep::Prepared);
    let receipt = job.publish().unwrap();
    assert_eq!(fixture.control.memory().used(), 0);
    assert_eq!(receipt.database, request.target());
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(fixture.path().join("database"))
        .unwrap();
    file.try_lock().unwrap();
    drop(file);
    std::fs::write(fixture.path().join("database"), b"later committed contents").unwrap();
    fixture.control.cancellation().cancel();
    assert_eq!(job.advance().unwrap(), BackupRestoreStep::Complete);
    assert_eq!(job.publish().unwrap(), receipt);
    assert_eq!(job.cancel().unwrap(), Some(receipt));
    assert_eq!(
        std::fs::read(fixture.path().join("database")).unwrap(),
        b"later committed contents"
    );
}

#[test]
fn scheduled_restore_failure_unwinding_and_drop_enter_recovery_without_reimport() {
    for failure in 0..3 {
        let fixture = Fixture::new();
        let factory = Factory {
            failure,
            ..Factory::default()
        };
        let mut job = fixture.begin().into_job(factory.clone()).unwrap();
        let id = job.id();
        let request = job.request();
        if failure == 0 {
            assert_eq!(job.advance().unwrap(), BackupRestoreStep::Pending);
        } else if failure == 1 {
            assert!(matches!(job.advance(), Err(BackupError::Integrity)));
            assert!(matches!(job.advance(), Err(BackupError::Finished)));
            assert_eq!(factory.closed.load(Ordering::SeqCst), 1);
        } else {
            assert!(
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| job.advance())).is_err()
            );
            assert!(matches!(job.advance(), Err(BackupError::Finished)));
            assert_eq!(factory.closed.load(Ordering::SeqCst), 1);
        }
        assert!(!fixture.path().exists());
        drop(job);
        assert_eq!(factory.closed.load(Ordering::SeqCst), 1);
        assert_eq!(fixture.control.memory().used(), 0);
        let operation = fixture
            .repository
            .resume_restore(id, fixture.path(), &fixture.control)
            .unwrap();
        let mut job = operation
            .into_job(Factory {
                failure: 0,
                ..factory.clone()
            })
            .unwrap();
        assert_eq!(job.request(), request);
        prepare(&mut job);
        assert_eq!(job.publish().unwrap().database, request.target());
        assert_eq!(factory.imported.load(Ordering::SeqCst), 1);
        assert_eq!(factory.recovered.load(Ordering::SeqCst), 1);
        assert_eq!(factory.closed.load(Ordering::SeqCst), 2);
    }
}

#[test]
fn scheduled_restore_cancellation_retires_active_work_and_persists_the_terminal_intent() {
    let fixture = Fixture::new();
    let factory = Factory::default();
    let operation = fixture.begin();
    let retained = fixture.control.memory().used();
    let mut job = operation.into_job(factory.clone()).unwrap();
    let id = job.id();
    fixture.control.cancellation().cancel();
    assert!(matches!(job.advance(), Err(BackupError::Cancelled(_))));
    assert_eq!(factory.closed.load(Ordering::SeqCst), 1);
    assert_eq!(fixture.control.memory().used(), retained);
    assert_eq!(job.cancel().unwrap(), None);
    assert_eq!(fixture.control.memory().used(), 0);
    assert!(matches!(job.advance(), Err(BackupError::Finished)));
    assert!(matches!(job.publish(), Err(BackupError::Finished)));
    drop(job);
    let operation = fixture
        .repository
        .resume_restore(id, fixture.path(), &fixture.control)
        .unwrap();
    assert!(matches!(
        operation.into_job(factory.clone()),
        Err(BackupError::Finished)
    ));
    assert_eq!(factory.imported.load(Ordering::SeqCst), 1);
    assert_eq!(factory.recovered.load(Ordering::SeqCst), 0);
    assert!(!fixture.path().exists());
    fixture
        .repository
        .remove_backup(fixture.backup, &StorageReadControl::with_limit(128 << 10))
        .unwrap();
}

#[test]
fn scheduled_restore_constructor_failure_preserves_recovery_admission() {
    let fixture = Fixture::new();
    let factory = Factory {
        failure: 3,
        ..Factory::default()
    };
    let operation = fixture.begin();
    let id = operation.id();
    let request = operation.request();
    assert!(matches!(
        operation.into_job(factory.clone()),
        Err(BackupError::Invalid("fixture constructor failed"))
    ));
    assert_eq!(fixture.control.memory().used(), 0);
    assert!(!fixture.path().exists());
    let operation = fixture
        .repository
        .resume_restore(id, fixture.path(), &fixture.control)
        .unwrap();
    assert_eq!(operation.request(), request);
    let mut job = operation
        .into_job(Factory {
            failure: 0,
            ..factory.clone()
        })
        .unwrap();
    prepare(&mut job);
    job.publish().unwrap();
    assert_eq!(factory.imported.load(Ordering::SeqCst), 1);
    assert_eq!(factory.recovered.load(Ordering::SeqCst), 1);
}

#[test]
fn scheduled_restore_resolves_prepared_and_published_markers_after_lost_replies() {
    for fault in [
        Fault::RestorePrepared,
        Fault::RestoreSync,
        Fault::RestoreComplete,
    ] {
        let fixture = Fixture::new();
        let factory = Factory::default();
        let mut job = fixture.begin().into_job(factory.clone()).unwrap();
        let id = job.id();
        if matches!(fault, Fault::RestorePrepared) {
            *fixture.repository.fault.lock() = Some(fault);
            loop {
                match job.advance() {
                    Ok(BackupRestoreStep::Pending) => (),
                    Err(_) => break,
                    _ => panic!("preparation fault did not fire"),
                }
            }
        } else {
            prepare(&mut job);
            *fixture.repository.fault.lock() = Some(fault);
            assert!(job.publish().is_err());
            std::fs::write(fixture.path().join("database"), b"later committed contents").unwrap();
        }
        drop(job);
        fixture
            .repository
            .remove_backup(fixture.backup, &fixture.control)
            .unwrap();
        let operation = fixture
            .repository
            .resume_restore(id, fixture.path(), &fixture.control)
            .unwrap();
        let mut job = operation.into_job(factory.clone()).unwrap();
        assert!(matches!(
            job.advance().unwrap(),
            BackupRestoreStep::Prepared | BackupRestoreStep::Complete
        ));
        job.publish().unwrap();
        assert_eq!(factory.imported.load(Ordering::SeqCst), 1);
        assert_eq!(factory.recovered.load(Ordering::SeqCst), 0);
        if !matches!(fault, Fault::RestorePrepared) {
            assert_eq!(
                std::fs::read(fixture.path().join("database")).unwrap(),
                b"later committed contents"
            );
        }
    }
}
