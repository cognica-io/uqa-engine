//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use std::process::Command;
use uqa_storage::{
    key_value::conformance::{
        diskann_rebuild_until_process_loss, kill_diskann_publication_owner,
        verify_diskann_recovered_publication, verify_diskann_recovered_records,
        verify_diskann_restore_source,
    },
    PersistentStorageProvider,
};

const PATH_ENV: &str = "UQA_DISKANN_PUBLICATION_PATH";
const COMMITTED_ENV: &str = "UQA_DISKANN_PUBLICATION_COMMITTED";

#[test]
fn diskann_publication_process_loss_recovers_complete_redb_generations_and_cleanup() {
    if let Some(path) = std::env::var_os(PATH_ENV) {
        let owner = crate::RedbStorage::open(path).unwrap();
        let session = owner.open_session().unwrap();
        diskann_rebuild_until_process_loss(
            &*session.backend,
            std::env::var(COMMITTED_ENV).unwrap() == "true",
        )
        .unwrap();
        return;
    }
    let (_, test) = concat!(
        module_path!(),
        "::diskann_publication_process_loss_recovers_complete_redb_generations_and_cleanup"
    )
    .split_once("::")
    .unwrap();
    for committed in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("publication-process.redb");
        let previous = {
            let owner = crate::RedbStorage::open(&path).unwrap();
            let session = owner.open_session().unwrap();
            verify_diskann_restore_source(&*session.backend).unwrap()
        };
        kill_diskann_publication_owner(
            Command::new(std::env::current_exe().unwrap())
                .args(["--exact", test, "--nocapture"])
                .env(PATH_ENV, &path)
                .env(COMMITTED_ENV, committed.to_string()),
        )
        .unwrap();
        let generation = {
            let owner = crate::RedbStorage::open(&path).unwrap();
            let session = owner.open_session().unwrap();
            let generation =
                verify_diskann_recovered_publication(&*session.backend, previous, committed)
                    .unwrap();
            session.backend.reclaim_obsolete().unwrap();
            verify_diskann_recovered_records(&owner.store(), generation).unwrap();
            generation
        };
        let owner = crate::RedbStorage::open(&path).unwrap();
        let session = owner.open_session().unwrap();
        assert_eq!(
            verify_diskann_recovered_publication(&*session.backend, previous, committed).unwrap(),
            generation
        );
        verify_diskann_recovered_records(&owner.store(), generation).unwrap();
    }
}
