//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

struct Step {
    action: Option<Box<dyn FnOnce()>>,
    closed: Arc<AtomicUsize>,
    calls: Arc<AtomicUsize>,
}

impl BackupRestoreWork for Step {
    fn advance(&mut self) -> BackupResult<BackupRestoreWorkStep> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if let Some(action) = self.action.take() {
            action();
        }
        Ok(BackupRestoreWorkStep::Complete)
    }
}

impl Drop for Step {
    fn drop(&mut self) {
        self.closed.fetch_add(1, Ordering::SeqCst);
    }
}

#[test]
fn scheduled_restore_path_rejects_unadmitted_names_and_releases_completed_work() {
    let temporary = tempfile::tempdir().unwrap();
    let pinned = Dir::open_ambient_dir(temporary.path(), cap_std::ambient_authority()).unwrap();
    let directory = RestoreDirectory { directory: &pinned };
    drop(directory.create_file("database").unwrap());
    pinned.create_dir("nested").unwrap();
    let control = StorageReadControl::with_limit(128 << 10);
    for name in [
        "",
        ".",
        "..",
        "../database",
        "nested/database",
        "/database",
        "nul\0byte",
        MARKER,
        "nested",
        "missing",
    ] {
        assert!(directory
            .begin_file_work::<Step>(name, &control, |_| panic!("invalid name reached provider"))
            .is_err());
        assert_eq!(control.memory().used(), 0);
    }
    #[cfg(unix)]
    {
        pinned.symlink("database", "link").unwrap();
        assert!(directory
            .begin_file_work::<Step>("link", &control, |_| panic!("symlink reached provider"))
            .is_err());
    }
    let closed = Arc::new(AtomicUsize::new(0));
    let calls = Arc::new(AtomicUsize::new(0));
    let mut work = directory
        .begin_file_work("database", &control, |path| {
            assert!(control.memory().used() >= path.as_os_str().len());
            Ok(Step {
                action: None,
                closed: closed.clone(),
                calls: calls.clone(),
            })
        })
        .unwrap();
    assert_eq!(work.advance().unwrap(), BackupRestoreWorkStep::Complete);
    assert_eq!(closed.load(Ordering::SeqCst), 1);
    assert_eq!(control.memory().used(), 0);
    control.cancellation().cancel();
    assert_eq!(work.advance().unwrap(), BackupRestoreWorkStep::Complete);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert!(directory
        .begin_file_work::<Step>("database", &control, |_| panic!("cancelled constructor"))
        .is_err());
    let low = StorageReadControl::with_limit(1);
    assert!(directory
        .begin_file_work::<Step>("database", &low, |_| panic!("unadmitted path allocation"))
        .is_err());
    assert_eq!(low.memory().used(), 0);
}

#[cfg(unix)]
#[test]
fn scheduled_restore_path_retires_before_reusing_a_moved_directory() {
    for during_step in [false, true] {
        let temporary = tempfile::tempdir().unwrap();
        let initial = temporary.path().join("initial");
        let moved = temporary.path().join("moved");
        std::fs::create_dir(&initial).unwrap();
        let pinned = Dir::open_ambient_dir(&initial, cap_std::ambient_authority()).unwrap();
        let directory = RestoreDirectory { directory: &pinned };
        drop(directory.create_file("database").unwrap());
        let control = StorageReadControl::with_limit(128 << 10);
        let closed = Arc::new(AtomicUsize::new(0));
        let calls = Arc::new(AtomicUsize::new(0));
        let from = initial.clone();
        let to = moved.clone();
        let action = move || {
            std::fs::rename(&from, &to).unwrap();
            std::fs::create_dir(&from).unwrap();
            std::fs::write(from.join("database"), b"unrelated file").unwrap();
        };
        let action = during_step.then(|| Box::new(action) as Box<dyn FnOnce()>);
        let mut work = directory
            .begin_file_work("database", &control, |_| {
                Ok(Step {
                    action,
                    closed: closed.clone(),
                    calls: calls.clone(),
                })
            })
            .unwrap();
        if !during_step {
            std::fs::rename(&initial, &moved).unwrap();
            std::fs::create_dir(&initial).unwrap();
            std::fs::write(initial.join("database"), b"unrelated file").unwrap();
        }
        assert!(work.advance().is_err());
        assert!(matches!(work.advance(), Err(BackupError::Finished)));
        assert_eq!(closed.load(Ordering::SeqCst), 1);
        assert_eq!(calls.load(Ordering::SeqCst), usize::from(during_step));
        assert_eq!(control.memory().used(), 0);
        assert_eq!(
            std::fs::read(initial.join("database")).unwrap(),
            b"unrelated file"
        );
        assert_eq!(std::fs::read(moved.join("database")).unwrap(), b"");
    }
}

#[test]
fn scheduled_restore_path_checks_nonregular_completion_and_closes_on_constructor_failure() {
    for nonregular in [false, true] {
        let temporary = tempfile::tempdir().unwrap();
        let pinned = Dir::open_ambient_dir(temporary.path(), cap_std::ambient_authority()).unwrap();
        let directory = RestoreDirectory { directory: &pinned };
        drop(directory.create_file("database").unwrap());
        let control = StorageReadControl::with_limit(128 << 10);
        assert!(matches!(
            directory
                .begin_file_work::<Step>("database", &control, |_| Err(BackupError::Integrity)),
            Err(BackupError::Integrity)
        ));
        assert_eq!(control.memory().used(), 0);
        let closed = Arc::new(AtomicUsize::new(0));
        let mut work = directory
            .begin_file_work("database", &control, |path| {
                let path = path.to_owned();
                Ok(Step {
                    action: Some(Box::new(move || {
                        std::fs::remove_file(&path).unwrap();
                        if nonregular {
                            std::fs::create_dir(&path).unwrap();
                        } else {
                            std::fs::write(&path, b"provider-owned replacement").unwrap();
                        }
                    })),
                    closed: closed.clone(),
                    calls: Arc::new(AtomicUsize::new(0)),
                })
            })
            .unwrap();
        if nonregular {
            assert!(work.advance().is_err());
            assert!(matches!(work.advance(), Err(BackupError::Finished)));
        } else {
            assert_eq!(work.advance().unwrap(), BackupRestoreWorkStep::Complete);
            assert_eq!(
                pinned.read("database").unwrap(),
                b"provider-owned replacement"
            );
        }
        assert_eq!(closed.load(Ordering::SeqCst), 1);
        assert_eq!(control.memory().used(), 0);
    }
}
