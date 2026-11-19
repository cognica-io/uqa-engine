//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::backup::{BackupCaptureStep, BackupLimits, BackupOptions, Protection};
use uqa_core::Value;

fn repository(path: &std::path::Path) -> LocalRepository {
    LocalRepository::create(
        path,
        Protection::Plaintext,
        BackupLimits::new(4096, 4096).unwrap(),
        &StorageReadControl::with_limit(1 << 20),
    )
    .unwrap()
}

fn capture(source: &Engine, repository: &LocalRepository, parent: Option<BackupId>) -> BackupRoot {
    let control = StorageReadControl::with_limit(32 << 20);
    let mut options = BackupOptions::new(32 << 20, 16 << 20);
    options.parent = parent;
    let root = finish_capture(source.begin_backup(repository, options, &control).unwrap());
    assert_eq!(control.memory().used(), 0);
    root
}

fn finish_capture(mut job: crate::backup::BackupJob<'_>) -> BackupRoot {
    while job.advance(8, 4096).unwrap() == BackupCaptureStep::Pending {}
    job.publish().unwrap()
}

fn retire_ancestor(repository: &LocalRepository, parent: BackupRoot, control: &StorageReadControl) {
    repository.remove_backup(parent.backup(), control).unwrap();
    for _ in 0..100 {
        let report = repository.collect_garbage(16, 16 << 20, control).unwrap();
        if report.remaining_files == 0 {
            return;
        }
    }
    panic!("ancestor collection did not finish");
}

fn begin<'a>(
    repository: &'a LocalRepository,
    root: BackupRoot,
    control: &StorageReadControl,
) -> RestoreJob<'a> {
    let authority = Engine::new();
    let job = authority
        .begin_restore(
            repository,
            root.backup(),
            RestoreDestination::Memory,
            RestoreOptions::new(16 << 20, 8 << 20),
            control,
        )
        .unwrap();
    drop(authority);
    job
}

fn assert_unusable(mut job: RestoreJob<'_>, expected: &str) {
    let error = loop {
        match job.advance(4096) {
            Err(error) => break error,
            Ok(RestorePhase::Reading | RestorePhase::Validating) => (),
            Ok(phase) => panic!("unusable restore input reached {phase:?}"),
        }
    };
    assert!(error.to_string().contains(expected), "{error}");
    assert_eq!(job.phase(), RestorePhase::Failed);
    assert!(matches!(job.publish(), Err(BackupError::Finished)));
}

fn prepare(job: &mut RestoreJob<'_>) {
    let mut validating = false;
    loop {
        match job.advance(1024).unwrap() {
            RestorePhase::Reading => (),
            RestorePhase::Validating => validating = true,
            RestorePhase::Prepared => break,
            phase => panic!("unexpected phase: {phase:?}"),
        }
    }
    assert!(validating);
}

#[test]
fn memory_restore_job_activates_full_and_incremental_images_after_source_retirement() {
    for incremental in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let repository = repository(&directory.path().join("repository"));
        let source = Engine::new();
        source.sql("CREATE TABLE items(id integer PRIMARY KEY, body text); CREATE SEQUENCE ticket START 41; INSERT INTO items VALUES(1,'first'); CREATE VIEW visible AS SELECT id,body FROM items", &[]).unwrap();
        let parent = capture(&source, &repository, None);
        let root = if incremental {
            source
                .sql(
                    "UPDATE items SET body='latest'; INSERT INTO items VALUES(2,'second')",
                    &[],
                )
                .unwrap();
            capture(&source, &repository, Some(parent.backup()))
        } else {
            parent
        };
        let expected = source
            .sql("SELECT id,body FROM visible ORDER BY id", &[])
            .unwrap()
            .rows;
        if incremental {
            let cleanup = StorageReadControl::with_limit(32 << 20);
            retire_ancestor(&repository, parent, &cleanup);
            assert_eq!(cleanup.memory().used(), 0);
        }
        let control = StorageReadControl::with_limit(32 << 20);
        let mut job = begin(&repository, root, &control);
        drop(source);
        assert_eq!(job.root(), Some(root));
        assert!(matches!(job.publish(), Err(BackupError::Finished)));
        assert_eq!(job.phase(), RestorePhase::Reading);
        prepare(&mut job);
        let restored = job.publish().unwrap();
        assert_eq!(job.phase(), RestorePhase::Complete);
        assert_eq!(
            restored
                .sql("SELECT id,body FROM visible ORDER BY id", &[])
                .unwrap()
                .rows,
            expected
        );
        restored
            .sql("INSERT INTO items VALUES(3,'after')", &[])
            .unwrap();
        restored
            .sql(
                "BEGIN; INSERT INTO items VALUES(4,'rolled back'); ROLLBACK",
                &[],
            )
            .unwrap();
        assert!(restored
            .sql("SELECT id FROM items WHERE id=4", &[])
            .unwrap()
            .rows
            .is_empty());
        assert_eq!(
            restored
                .sql("SELECT nextval('ticket') AS n", &[])
                .unwrap()
                .rows[0]["n"],
            Value::Int(41)
        );
        let repeated = job.publish().unwrap();
        assert!(RetainedValue::ptr_eq(&restored.0, &repeated.0));
        assert_eq!(
            repeated
                .sql("SELECT body FROM items WHERE id=3", &[])
                .unwrap()
                .rows[0]["body"],
            Value::Str("after".into())
        );
        control.cancellation().cancel();
        assert_eq!(job.advance(0).unwrap(), RestorePhase::Complete);
        let late = job.cancel().unwrap().unwrap();
        assert!(RetainedValue::ptr_eq(&restored.0, &late.0));
        drop(job);
        drop(repeated);
        drop(late);
        restored
            .sql("INSERT INTO items VALUES(5,'still live')", &[])
            .unwrap();
        let next_root = capture(&restored, &repository, None);
        assert_ne!(next_root.cut().source, root.cut().source);
        drop(restored);
        assert_eq!(control.memory().used(), 0);
    }
}

#[test]
fn memory_restore_job_cancel_drop_and_failed_steps_retire_unpublished_state() {
    let directory = tempfile::tempdir().unwrap();
    let repository = repository(&directory.path().join("repository"));
    let source = Engine::new();
    source
        .sql(
            "CREATE TABLE items(value integer); INSERT INTO items VALUES(1)",
            &[],
        )
        .unwrap();
    let root = capture(&source, &repository, None);
    drop(source);
    for mode in [
        "drop",
        "cancel",
        "prepared",
        "token",
        "invalid_window",
        "catalog_cancel",
        "catalog_drop",
    ] {
        let control = StorageReadControl::with_limit(32 << 20);
        let mut job = begin(&repository, root, &control);
        for _ in 0..4 {
            job.advance(1024).unwrap();
        }
        assert!(control.memory().used() > 0);
        if matches!(mode, "catalog_cancel" | "catalog_drop") {
            while !matches!(
                job.state,
                State::Validating(_, CatalogValidation::Relationships(_), _)
            ) {
                assert_ne!(job.advance(1024).unwrap(), RestorePhase::Prepared);
            }
            assert_eq!(job.advance(1024).unwrap(), RestorePhase::Validating);
            assert!(matches!(job.publish(), Err(BackupError::Finished)));
        }
        match mode {
            "drop" | "catalog_drop" => (),
            "cancel" | "prepared" | "catalog_cancel" => {
                if mode == "prepared" {
                    prepare(&mut job);
                }
                assert!(job.cancel().unwrap().is_none());
                assert_eq!(job.phase(), RestorePhase::Cancelled);
                assert!(job.cancel().unwrap().is_none());
                assert!(matches!(job.advance(1024), Err(BackupError::Finished)));
                assert!(matches!(job.publish(), Err(BackupError::Finished)));
            }
            _ => {
                if mode == "token" {
                    control.cancellation().cancel();
                }
                assert!(job
                    .advance(if mode == "invalid_window" { 0 } else { 1024 })
                    .is_err());
                assert_eq!(job.phase(), RestorePhase::Failed);
                assert!(job.cancel().unwrap().is_none());
                assert_eq!(job.phase(), RestorePhase::Failed);
                assert!(matches!(job.advance(1024), Err(BackupError::Finished)));
                assert!(matches!(job.publish(), Err(BackupError::Finished)));
            }
        }
        drop(job);
        assert_eq!(control.memory().used(), 0, "{mode}");
    }
}

#[test]
fn memory_restore_job_validation_failure_retires_the_candidate_and_archive_lease() {
    let directory = tempfile::tempdir().unwrap();
    let repository = repository(&directory.path().join("repository"));
    let source = Engine::new();
    source.sql("CREATE TABLE items(id integer); INSERT INTO items VALUES(1); CREATE VIEW visible AS SELECT id FROM public.items", &[]).unwrap();
    source
        .storage
        .tables
        .write()
        .remove(&uqa_core::RelationIdentity::new("public", "items"))
        .unwrap()
        .unwrap();
    let root = capture(&source, &repository, None);
    drop(source);
    let control = StorageReadControl::with_limit(32 << 20);
    let mut job = begin(&repository, root, &control);
    while job.phase() == RestorePhase::Reading {
        job.advance(1024).unwrap();
    }
    assert_eq!(job.phase(), RestorePhase::Validating);
    assert_eq!(job.advance(1024).unwrap(), RestorePhase::Validating);
    let error = loop {
        match job.advance(1024) {
            Ok(RestorePhase::Validating) => {
                assert!(matches!(job.publish(), Err(BackupError::Finished)));
            }
            Ok(phase) => panic!("unresolved view reached {phase:?}"),
            Err(error) => break error,
        }
    };
    assert!(
        error.to_string().contains("missing native plan relation"),
        "{error}"
    );
    assert_eq!(job.phase(), RestorePhase::Failed);
    assert_eq!(control.memory().used(), 0);
    assert!(matches!(job.publish(), Err(BackupError::Finished)));
    assert!(matches!(job.advance(1024), Err(BackupError::Finished)));
    assert!(job.cancel().unwrap().is_none());
    assert_eq!(job.phase(), RestorePhase::Failed);
    repository.remove_backup(root.backup(), &control).unwrap();
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn memory_restore_job_admission_preserves_authority_and_transaction_failures() {
    let directory = tempfile::tempdir().unwrap();
    let repository = repository(&directory.path().join("repository"));
    let source = Engine::new();
    let root = capture(&source, &repository, None);
    let authority = Engine::new();
    authority
        .sql("CREATE ROLE reader; SET ROLE reader", &[])
        .unwrap();
    let control = StorageReadControl::with_limit(32 << 20);
    let options = RestoreOptions::new(16 << 20, 8 << 20);
    let rejected = authority.begin_restore(
        &repository,
        root.backup(),
        RestoreDestination::Memory,
        options,
        &control,
    );
    let Err(error) = rejected else {
        panic!("unauthorized restore")
    };
    assert_eq!(error.sqlstate(), Some("42501"));
    assert_eq!(control.memory().used(), 0);
    authority.sql("RESET ROLE; BEGIN", &[]).unwrap();
    assert!(authority.sql("SELECT 1/0", &[]).is_err());
    let rejected = authority.begin_restore(
        &repository,
        root.backup(),
        RestoreDestination::Memory,
        options,
        &control,
    );
    let Err(error) = rejected else {
        panic!("failed transaction admitted restore")
    };
    assert_eq!(error.sqlstate(), Some("25P02"));
    assert_eq!(control.memory().used(), 0);
    authority.sql("ROLLBACK", &[]).unwrap();
    let denied = StorageReadControl::with_limit(0);
    assert!(authority
        .begin_restore(
            &repository,
            root.backup(),
            RestoreDestination::Memory,
            options,
            &denied
        )
        .is_err());
    assert_eq!(denied.memory().used(), 0);
}

mod analyzer_resources;
mod application_priority;
mod constraint_bindings;
mod external_functions;
mod foreign_bindings;
mod foreign_callbacks;
mod function_origins;
mod graph_path_indexes;
mod index_bindings;
mod legacy_graph_path_indexes;
mod models;
mod partition_bindings;
mod persistent;
mod providers;
mod routine_bindings;
mod routine_bodies;
mod routine_declarations;
mod routine_types;
mod rule_bindings;
