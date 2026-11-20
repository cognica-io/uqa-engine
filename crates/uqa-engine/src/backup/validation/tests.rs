//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::tests::providers::Provider;

#[rstest::rstest]
#[case::memory(Provider::Memory)]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn catalog_validation_keeps_the_selected_missing_namespace_after_the_live_table_is_removed(
    #[case] provider: Provider,
) {
    let directory = tempfile::tempdir().unwrap();
    let engine = std::sync::Arc::new(provider.open(&directory.path().join("selection")));
    engine.sql("CREATE SCHEMA original; CREATE TABLE original.item(id integer); CREATE TABLE keep(id integer)", &[]).unwrap();
    let complete = engine.durable.schemas.read().clone();
    assert!(engine.durable.schemas.write().remove("original").unwrap());
    let control = StorageReadControl::with_limit(32 << 20);
    let mut checks = CatalogValidation::begin(&engine, &control).unwrap();
    *engine.durable.schemas.write() = complete;
    engine.sql("DROP TABLE original.item", &[]).unwrap();

    let mut polls = 0;
    while matches!(checks, CatalogValidation::Capture(_)) {
        checks = checks
            .advance(&engine, |owner| owner, &control, 1)
            .unwrap()
            .unwrap();
        polls += 1;
    }
    assert!(polls > 2);
    let error = loop {
        match checks.advance(&engine, |owner| owner, &control, 1) {
            Ok(Some(next)) => checks = next,
            Ok(None) => panic!("validation accepted the original table without its schema"),
            Err(error) => break error,
        }
    };
    assert!(
        error
            .to_string()
            .contains("missing native catalog namespace"),
        "{error}"
    );
    assert_eq!(control.memory().used(), 0);

    let mut remaining = Some(CatalogValidation::begin(&engine, &control).unwrap());
    while let Some(work) = remaining {
        remaining = work.advance(&engine, |owner| owner, &control, 64).unwrap();
    }
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn catalog_validation_preserves_resource_errors_and_sql_diagnostics() {
    use uqa_core::memory::MemoryError;
    use uqa_storage::backup::BackupError;

    let reason = uqa_core::QueryCancelled::USER_REQUEST;
    let convert = |error: SQLError| {
        BackupError::from(sql_error(
            error.with_context("checking a stored catalog binding"),
        ))
    };
    assert!(matches!(convert(reason.into()), BackupError::Cancelled(actual) if actual == reason));
    let memory = MemoryBudget::new(11);
    let held = memory.reserve(7).unwrap();
    let denied = memory.reserve(5).unwrap_err();
    assert!(matches!(
        convert(denied.into()),
        BackupError::Memory(MemoryError::Limit {
            required: 12,
            limit: 11
        })
    ));
    drop(held);
    assert_eq!(memory.used(), 0);

    let raised = convert(SQLError::Routine {
        sqlstate: "53200".into(),
        message: "user-defined diagnostic".into(),
    });
    assert!(matches!(raised, BackupError::Storage(_)));
    assert!(raised.to_string().contains("user-defined diagnostic"));
}
