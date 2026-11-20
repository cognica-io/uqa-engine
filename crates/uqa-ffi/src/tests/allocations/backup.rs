//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{fixtures::Database, target};
use crate::*;
use std::{ffi::CString, ptr};

#[test]
fn archive_handles_precede_durable_admission_and_publication_can_retry() {
    let database = Database::new(UQA_OPEN_MEMORY);
    database.sql(c"CREATE TABLE archive_value(id bigint); INSERT INTO archive_value VALUES (11)");
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("repository");
    let text = CString::new(path.to_str().unwrap()).unwrap();
    let mut repo = ptr::null_mut();
    let mut capture = ptr::null_mut();
    let mut restore = ptr::null_mut();
    let mut restored = ptr::null_mut();
    let mut id = UQABackupId::default();
    unsafe {
        let mut create =
            || uqa_backup_repository_create(text.as_ptr(), ptr::null(), ptr::null(), &raw mut repo);
        assert_eq!(
            target::reject::<UQABackupRepository, _>(&mut create),
            UQA_ERROR_OUT_OF_MEMORY
        );
        assert!(!path.exists());
        assert_eq!(create(), UQA_OK);
        let operations = path.join("operations");
        let initial = std::fs::read_dir(&operations).unwrap().count();
        let mut begin = || {
            uqa_backup_job_create(
                database.engine,
                repo,
                ptr::null(),
                ptr::null(),
                &raw mut capture,
            )
        };
        assert_eq!(
            target::reject::<UQABackupJob, _>(&mut begin),
            UQA_ERROR_OUT_OF_MEMORY
        );
        assert_eq!(std::fs::read_dir(operations).unwrap().count(), initial);
        assert_eq!(begin(), UQA_OK);
        assert_eq!(uqa_backup_job_finish(capture, &raw mut id), UQA_OK);
        assert_eq!(uqa_backup_job_destroy(&raw mut capture), UQA_OK);
        let mut begin = || {
            uqa_restore_job_create(
                database.engine,
                repo,
                &raw const id,
                ptr::null(),
                UQA_OPEN_MEMORY,
                ptr::null_mut(),
                ptr::null(),
                &raw mut restore,
            )
        };
        assert_eq!(
            target::reject::<UQARestoreJob, _>(&mut begin),
            UQA_ERROR_OUT_OF_MEMORY
        );
        assert_eq!(begin(), UQA_OK);
        assert_eq!(uqa_backup_repository_close(&raw mut repo), UQA_OK);
        assert_eq!(
            target::reject::<UQAEngine, _>(|| uqa_restore_job_finish(
                &raw mut restore,
                &raw mut restored
            )),
            UQA_ERROR_OUT_OF_MEMORY
        );
        assert!(restored.is_null());
        assert!(!restore.is_null());
        assert_eq!(
            uqa_restore_job_finish(&raw mut restore, &raw mut restored),
            UQA_OK
        );
        assert!(restore.is_null());
        assert_eq!(
            uqa_engine_execute(
                restored,
                c"SELECT id FROM archive_value".as_ptr(),
                database.result
            ),
            UQA_OK
        );
        assert_eq!(database.retained_integer(), 11);
        database.sql(c"SELECT 1");
        assert_eq!(uqa_engine_close(&raw mut restored), UQA_OK);
    }
}
