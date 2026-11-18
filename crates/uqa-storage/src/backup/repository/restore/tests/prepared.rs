//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn staged_resource_validation_releases_handles_on_every_terminal_transition() {
    for outcome in ["complete", "failed", "cancelled", "dropped"] {
        let fixture = Fixture::new();
        let mut initial = fixture.begin();
        let id = initial.id();
        initial.prepare(Importer::default()).unwrap();
        drop(initial);
        fixture
            .repository
            .remove_backup(fixture.backup, &fixture.control)
            .unwrap();
        let resume = || {
            fixture
                .repository
                .resume_restore(id, fixture.path(), &fixture.control)
                .unwrap()
        };
        let live = Arc::new(());
        let stage = resume()
            .begin_prepared_validation(|path, request, _| {
                let mut file = std::fs::File::open(path.join("database"))?;
                let mut identity = [0; 16];
                file.read_exact(&mut identity)?;
                assert_eq!(identity, request.target().as_bytes());
                Ok((file, Arc::clone(&live)))
            })
            .unwrap();
        let PreparedRestoreStep::Pending(work) = stage else {
            panic!("unpublished database")
        };
        let stage = work
            .advance(|_| Ok(BackupRestoreWorkStep::Pending))
            .unwrap();
        let PreparedRestoreStep::Pending(work) = stage else {
            panic!("unfinished validation")
        };
        assert_eq!(Arc::strong_count(&live), 2);
        assert!(!fixture.path().exists());
        match outcome {
            "failed" => {
                assert!(matches!(
                    work.advance(|_| Err(BackupError::Integrity)),
                    Err(BackupError::Integrity)
                ));
                assert!(resume().cancel().unwrap().is_none());
            }
            "cancelled" => assert!(work.cancel().unwrap().is_none()),
            "dropped" => {
                drop(work);
                assert!(resume().cancel().unwrap().is_none());
            }
            "complete" => {
                let stage = work
                    .advance(|_| Ok(BackupRestoreWorkStep::Complete))
                    .unwrap();
                let PreparedRestoreStep::Complete(mut operation) = stage else {
                    panic!("validated database")
                };
                assert_eq!(Arc::strong_count(&live), 1);
                operation.publish().unwrap();
                drop(operation);
                let published = resume()
                    .begin_prepared_validation::<()>(|_, _, _| {
                        panic!("completed request reopened private files")
                    })
                    .unwrap();
                assert!(matches!(published, PreparedRestoreStep::Complete(_)));
            }
            _ => unreachable!(),
        }
        assert_eq!(Arc::strong_count(&live), 1);
        assert_eq!(fixture.control.memory().used(), 0);
    }
}
