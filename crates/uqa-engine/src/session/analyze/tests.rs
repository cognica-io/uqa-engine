//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Column-statistics visibility follows the caller's transaction and savepoints.

use super::*;

fn sessions(provider: usize) -> (tempfile::TempDir, Engine, Engine) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("statistics.db");
    let writer = match provider {
        0 => Engine::open(&path).unwrap(),
        1 => Engine::from_persistent_provider(Arc::new(
            uqa_storage_sqlite::SQLiteKeyValueStorage::open(&path).unwrap(),
        ))
        .unwrap(),
        2 => Engine::from_persistent_provider(Arc::new(
            uqa_storage_redb::RedbStorage::open(&path).unwrap(),
        ))
        .unwrap(),
        _ => unreachable!(),
    };
    let reader = writer.new_session().unwrap();
    for engine in [&writer, &reader] {
        engine.release_automatic_statistics_client();
        engine
            .session
            .statistics_worker
            .store(true, Ordering::Release);
    }
    writer
        .sql(
            "CREATE TABLE t (v INTEGER); INSERT INTO t VALUES (10); ANALYZE t",
            &[],
        )
        .unwrap();
    (directory, writer, reader)
}

fn persisted_rows(engine: &Engine) -> i64 {
    engine
        .storage
        .catalog
        .as_ref()
        .unwrap()
        .load_column_stats("public.t")
        .unwrap()[0]
        .row_count
}

#[test]
fn column_statistics_are_private_until_commit_and_restore_on_rollback() {
    // PostgreSQL 18.4 keeps pg_statistic changes private and transactional; its separately updated pg_class.reltuples is not evidence of column-statistics publication.
    for provider in 0..3 {
        let (_directory, writer, reader) = sessions(provider);
        writer
            .sql("BEGIN; INSERT INTO t VALUES (20); ANALYZE t", &[])
            .unwrap();
        assert_eq!(persisted_rows(&writer), 2);
        assert_eq!(persisted_rows(&reader), 1);
        assert_eq!(
            reader.column_stats("t").unwrap()["v"].max_value,
            Some(Value::Int(10))
        );
        writer.sql("ROLLBACK", &[]).unwrap();
        assert_eq!(persisted_rows(&writer), 1);
        assert_eq!(
            writer.column_stats("t").unwrap()["v"].max_value,
            Some(Value::Int(10))
        );

        writer
            .sql("BEGIN; INSERT INTO t VALUES (30); ANALYZE t", &[])
            .unwrap();
        assert_eq!(persisted_rows(&reader), 1);
        writer.sql("COMMIT", &[]).unwrap();
        assert_eq!(persisted_rows(&reader), 2);
        assert_eq!(
            reader.column_stats("t").unwrap()["v"].max_value,
            Some(Value::Int(30))
        );
    }
}

#[test]
fn read_only_analysis_commits_and_rolls_back_with_its_catalog_transaction() {
    for provider in 0..3 {
        let (_directory, writer, reader) = sessions(provider);
        writer.sql("INSERT INTO t VALUES (20)", &[]).unwrap();
        reader.sql("BEGIN READ ONLY; ANALYZE t", &[]).unwrap();
        assert_eq!(persisted_rows(&reader), 2);
        assert_eq!(persisted_rows(&writer), 1);
        reader.sql("ROLLBACK", &[]).unwrap();
        assert_eq!(persisted_rows(&reader), 1);
        reader
            .sql("BEGIN READ ONLY; ANALYZE t; COMMIT", &[])
            .unwrap();
        assert_eq!(persisted_rows(&writer), 2);

        writer.sql("INSERT INTO t VALUES (30)", &[]).unwrap();
        reader
            .sql("BEGIN READ ONLY; SAVEPOINT before_analysis; ANALYZE t", &[])
            .unwrap();
        assert_eq!(persisted_rows(&reader), 3);
        reader
            .sql("ROLLBACK TO before_analysis; COMMIT", &[])
            .unwrap();
        assert_eq!(persisted_rows(&writer), 2);

        reader.sql("BEGIN READ ONLY; ANALYZE t", &[]).unwrap();
        let error = reader.sql("INSERT INTO t VALUES (40)", &[]).unwrap_err();
        assert_eq!(error.sqlstate(), Some("25006"));
        reader.sql("ROLLBACK", &[]).unwrap();
        assert_eq!(persisted_rows(&writer), 2);
    }
}

#[test]
fn read_only_analysis_preserves_write_admission_through_savepoint_completion() {
    for provider in 0..3 {
        for completion in ["release", "rollback", "recover"] {
            let (_directory, writer, reader) = sessions(provider);
            writer.sql("INSERT INTO t VALUES (20)", &[]).unwrap();
            reader
                .sql("BEGIN READ ONLY; SAVEPOINT analyzed; ANALYZE t", &[])
                .unwrap();
            assert_eq!(persisted_rows(&reader), 2);
            if completion == "recover" {
                let error = reader.sql("SELECT 1 / 0", &[]).unwrap_err();
                assert_eq!(error.sqlstate(), Some("22012"));
                assert!(reader.transaction_failed());
            }
            if completion != "release" {
                reader.sql("ROLLBACK TO analyzed", &[]).unwrap();
            }
            reader.sql("RELEASE analyzed", &[]).unwrap();
            assert!(reader.current_transaction_is_read_only());
            reader.sql("COMMIT", &[]).unwrap();
            assert_eq!(
                persisted_rows(&writer),
                if completion == "release" { 2 } else { 1 },
                "provider {provider}, {completion}"
            );
            reader.sql("BEGIN READ ONLY", &[]).unwrap();
            let error = reader.sql("INSERT INTO t VALUES (30)", &[]).unwrap_err();
            assert_eq!(error.sqlstate(), Some("25006"));
            reader.sql("ROLLBACK", &[]).unwrap();
        }
    }
}

#[test]
fn read_only_analysis_preserves_write_admission_after_nested_transaction_completion() {
    for provider in 0..3 {
        for commit_child in [false, true] {
            let (_directory, writer, reader) = sessions(provider);
            writer.sql("INSERT INTO t VALUES (20)", &[]).unwrap();
            reader.sql("BEGIN READ ONLY", &[]).unwrap();
            reader.begin().unwrap();
            reader.run_analyze(Some("t")).unwrap();
            if commit_child {
                reader.commit().unwrap();
            } else {
                reader.rollback().unwrap();
            }
            assert!(reader.current_transaction_is_read_only());
            reader.commit().unwrap();
            assert_eq!(persisted_rows(&writer), if commit_child { 2 } else { 1 });
        }
    }
}

#[test]
fn analysis_and_a_later_row_write_preserve_both_commits_and_pending_maintenance() {
    for provider in 0..3 {
        let (_directory, writer, analyst) = sessions(provider);
        analyst.sql("BEGIN; ANALYZE t", &[]).unwrap();
        // Fix the publication order: this INSERT commits after collection while ANALYZE still owns private catalog changes.
        writer.sql("INSERT INTO t VALUES (20)", &[]).unwrap();
        analyst.sql("COMMIT", &[]).unwrap();
        assert_eq!(persisted_rows(&writer), 1);
        assert!(crate::statistics::MaintenanceState::load(
            writer.storage.catalog.as_deref().unwrap(),
            "public.t"
        )
        .unwrap()
        .invalidates_existing_statistics());
        assert_eq!(writer.column_stats("t").unwrap()["v"].row_count, 2);
    }
}

#[test]
fn nested_analysis_resets_parent_maintenance_only_when_the_child_commits() {
    for provider in 0..3 {
        for commit_child in [false, true] {
            let (_directory, writer, reader) = sessions(provider);
            writer.begin().unwrap();
            writer.sql("INSERT INTO t VALUES (20)", &[]).unwrap();
            writer.begin().unwrap();
            writer.run_analyze(Some("t")).unwrap();
            if commit_child {
                writer.commit().unwrap();
            } else {
                writer.rollback().unwrap();
            }
            writer.commit().unwrap();
            let state = crate::statistics::MaintenanceState::load(
                reader.storage.catalog.as_deref().unwrap(),
                "public.t",
            )
            .unwrap();
            assert_eq!(state.dirty(), !commit_child);
            assert_eq!(persisted_rows(&reader), if commit_child { 2 } else { 1 });
            assert_eq!(reader.column_stats("t").unwrap()["v"].row_count, 2);
        }
    }
}
