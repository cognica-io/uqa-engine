//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use uqa_storage::{notifications::conformance::*, PersistentStorageProvider};
use uqa_storage_redb::RedbStorage;

#[test]
fn notification_publication_survives_reopen_and_acknowledgement_is_conditional() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("notification-publication.redb");
    {
        let provider = RedbStorage::open(&path).unwrap();
        verify_publication_sessions(
            &provider.open_session().unwrap(),
            &provider.open_session().unwrap(),
        )
        .unwrap();
    }
    {
        let reopened = RedbStorage::open(&path).unwrap();
        verify_publication_reopen(&reopened.open_session().unwrap()).unwrap();
    }
    let reopened = RedbStorage::open(&path).unwrap();
    verify_publication_cleared(&reopened.open_session().unwrap()).unwrap();
}
