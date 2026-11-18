//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{backup::validation::CatalogValidation, tests::providers::Provider};

#[rstest::rstest]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn persistent_validation_holds_the_private_read_transaction_until_completion_or_cancellation(
    #[case] provider: Provider,
) {
    let directory = tempfile::tempdir().unwrap();
    let source = provider.open(&directory.path().join("catalog"));
    source
        .sql(
            "CREATE TABLE entry(id integer); CREATE VIEW records AS SELECT id FROM entry",
            &[],
        )
        .unwrap();
    for outcome in ["complete", "cancel", "drop"] {
        let control = StorageReadControl::with_limit(32 << 20);
        let provider = Arc::clone(source.storage.provider.as_ref().unwrap());
        let limits = catalog_limits(RestoreOptions::new(16 << 20, 8 << 20));
        let mut work =
            Validation::new(provider, MemoryBudget::new(16 << 20), limits, &RestoreFunctions::default(), &control).unwrap();
        let backend = loop {
            if let Some(ValidationStage::Relationships(
                session,
                CatalogValidation::Relationships(_),
            )) = &work.stage
            {
                break Arc::clone(session.engine().storage.backend.as_ref().unwrap());
            }
            assert_eq!(
                work.advance(64).unwrap(),
                BackupImageValidationStep::Pending
            );
        };
        for _ in 0..3 {
            assert!(backend.in_transaction());
            assert_eq!(work.advance(1).unwrap(), BackupImageValidationStep::Pending);
        }
        match outcome {
            "complete" => {
                while work.advance(1).unwrap() == BackupImageValidationStep::Pending {
                    assert!(backend.in_transaction());
                }
            }
            "cancel" => {
                control.cancellation().cancel();
                assert!(matches!(
                    work.advance(1),
                    Err(uqa_storage::backup::BackupError::Cancelled(_))
                ));
            }
            "drop" => (),
            _ => unreachable!(),
        }
        drop(work);
        assert!(!backend.in_transaction());
        assert_eq!(control.memory().used(), 0);
    }
    source.sql("INSERT INTO entry VALUES (1)", &[]).unwrap();
}
