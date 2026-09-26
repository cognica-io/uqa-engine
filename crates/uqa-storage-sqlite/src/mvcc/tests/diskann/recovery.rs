//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use std::process::Command;
use uqa_storage::key_value::conformance::{
    diskann_rebuild_until_process_loss, kill_diskann_publication_owner,
    verify_diskann_recovered_publication, verify_diskann_recovered_records,
    verify_diskann_restore_source,
};

const PATH_ENV: &str = "UQA_DISKANN_PUBLICATION_PATH";
const MODE_ENV: &str = "UQA_DISKANN_PUBLICATION_MODE";
const NATIVE_ENV: &str = "UQA_DISKANN_PUBLICATION_NATIVE";
const COMMITTED_ENV: &str = "UQA_DISKANN_PUBLICATION_COMMITTED";

#[test]
fn diskann_publication_process_loss_recovers_complete_sqlite_generations_and_cleanup() {
    if let Some(path) = std::env::var_os(PATH_ENV) {
        let connection = connection(
            Path::new(&path),
            std::env::var(MODE_ENV).unwrap().parse().unwrap(),
        );
        let (backend, _) = backend(&connection, std::env::var(NATIVE_ENV).unwrap() == "true");
        diskann_rebuild_until_process_loss(
            &*backend,
            std::env::var(COMMITTED_ENV).unwrap() == "true",
        )
        .unwrap();
        return;
    }
    let (_, test) = concat!(
        module_path!(),
        "::diskann_publication_process_loss_recovers_complete_sqlite_generations_and_cleanup"
    )
    .split_once("::")
    .unwrap();
    for mode in 0..4 {
        for native in [false, true] {
            for committed in [false, true] {
                let directory = tempfile::tempdir().unwrap();
                let path = directory.path().join("publication-process.db");
                let previous = {
                    let connection = connection(&path, mode);
                    let (backend, _) = backend(&connection, native);
                    verify_diskann_restore_source(&*backend).unwrap()
                };
                kill_diskann_publication_owner(
                    Command::new(std::env::current_exe().unwrap())
                        .args(["--exact", test, "--nocapture"])
                        .env(PATH_ENV, &path)
                        .env(MODE_ENV, mode.to_string())
                        .env(NATIVE_ENV, native.to_string())
                        .env(COMMITTED_ENV, committed.to_string()),
                )
                .unwrap();
                let generation = {
                    let connection = connection(&path, mode);
                    let (backend, records) = backend(&connection, native);
                    let generation =
                        verify_diskann_recovered_publication(&*backend, previous, committed)
                            .unwrap();
                    backend.reclaim_obsolete().unwrap();
                    verify_diskann_recovered_records(&*records, generation).unwrap();
                    generation
                };
                let reopened = connection(&path, mode);
                let (backend, records) = backend(&reopened, native);
                assert_eq!(
                    verify_diskann_recovered_publication(&*backend, previous, committed).unwrap(),
                    generation
                );
                verify_diskann_recovered_records(&*records, generation).unwrap();
            }
        }
    }
}
