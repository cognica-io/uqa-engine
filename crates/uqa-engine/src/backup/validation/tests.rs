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
    let engine = provider.open(&directory.path().join("selection"));
    engine.sql("CREATE SCHEMA original; CREATE TABLE original.item(id integer); CREATE TABLE keep(id integer)", &[]).unwrap();
    let complete = engine.durable.schemas.read().clone();
    assert!(engine.durable.schemas.write().remove("original").unwrap());
    let control = StorageReadControl::with_limit(32 << 20);
    let mut checks = CatalogValidation::begin(&engine, &control).unwrap();
    *engine.durable.schemas.write() = complete;
    engine.sql("DROP TABLE original.item", &[]).unwrap();

    let mut polls = 0;
    while matches!(checks, CatalogValidation::Capture(_)) {
        checks = checks.advance(&engine, &control, 1).unwrap().unwrap();
        polls += 1;
    }
    assert!(polls > 2);
    let Err(error) = checks.advance(&engine, &control, 1) else {
        panic!("validation accepted the original table without its schema");
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
        remaining = work.advance(&engine, &control, 64).unwrap();
    }
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn catalog_validation_returns_the_original_cancellation_reason_from_sql() {
    let reason = uqa_core::QueryCancelled::USER_REQUEST;
    let sql = uqa_sql::SQLError::from(reason).with_context("checking a stored catalog binding");
    let error = uqa_storage::backup::BackupError::from(sql_error(sql));
    assert!(
        matches!(error, uqa_storage::backup::BackupError::Cancelled(actual) if actual == reason)
    );
}
