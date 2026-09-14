//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use std::sync::Arc;
use uqa_analysis::whitespace_analyzer;
use uqa_storage::KeyValueInvertedIndex;
use uqa_storage_redb::RedbStorage;

#[path = "../../../uqa-storage/tests/cases/japanese/contract.rs"]
mod contract;

#[test]
fn japanese_occurrences_restore_from_redb_backup_with_original_database_removed() {
    for case in contract::cases() {
        let source = tempfile::tempdir().unwrap();
        let backup = tempfile::tempdir().unwrap();
        let source_path = source.path().join("source.redb");
        let backup_path = backup.path().join("restored.redb");
        {
            let storage = RedbStorage::open(&source_path).unwrap();
            let mut index = KeyValueInvertedIndex::new(
                Arc::new(storage.store()),
                "docs",
                whitespace_analyzer(),
            );
            contract::populate(&mut index, &case);
            contract::verify(&index, &case);
        }
        std::fs::copy(&source_path, &backup_path).unwrap();
        source.close().unwrap();
        assert!(!source_path.exists());
        let storage = RedbStorage::open(&backup_path).unwrap();
        let mut index =
            KeyValueInvertedIndex::new(Arc::new(storage.store()), "docs", whitespace_analyzer());
        contract::restore(&mut index, &case);
        contract::verify(&index, &case);
    }
}
