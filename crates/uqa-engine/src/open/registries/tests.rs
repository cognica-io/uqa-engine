//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use crate::{open::CatalogRestoreMode, tests::providers::Provider};
use std::sync::Arc;
use uqa_core::memory::MemoryBudget;
use uqa_execution::catalog::comments::CommentRegistry;

#[rstest::rstest]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn native_comment_restore_preserves_original_root_and_metadata_on_admission_failure(
    #[case] provider: Provider,
) {
    let directory = tempfile::tempdir().unwrap();
    let engine = provider.open(&directory.path().join("comments.db"));
    engine
        .sql(
            "CREATE TYPE saved AS ENUM ('one'); COMMENT ON TYPE saved IS 'original 한글'",
            &[],
        )
        .unwrap();
    let memory = MemoryBudget::new(1 << 20);
    let mut admitted = CommentRegistry::with_budget(&memory);
    for (address, text) in engine.durable.comments.read().iter() {
        admitted.insert(*address, text).unwrap();
    }
    *engine.durable.comments.write() = admitted;
    let original = engine.durable.comments.snapshot();
    let catalog = engine.storage.catalog.as_ref().unwrap();
    let stored = catalog.metadata_with_prefix("uqa.sql.comment.v1:").unwrap();
    let baseline = memory.used();
    let occupied = memory.reserve(memory.available()).unwrap();
    let error = engine
        .restore_engine_registries_from_catalog(catalog.as_ref(), CatalogRestoreMode::LoadOnly)
        .unwrap_err();
    assert_eq!(
        uqa_sql::catalog::errors::storage_error("comment restore", &error).sqlstate(),
        Some("53200"),
        "{provider:?}: {error}"
    );
    assert!(Arc::ptr_eq(&original, &engine.durable.comments.snapshot()));
    assert_eq!(
        catalog.metadata_with_prefix("uqa.sql.comment.v1:").unwrap(),
        stored
    );
    drop(occupied);
    assert_eq!(memory.used(), baseline);
    engine
        .restore_engine_registries_from_catalog(catalog.as_ref(), CatalogRestoreMode::LoadOnly)
        .unwrap();
    assert_eq!(*engine.durable.comments.read(), *original);
    engine
        .sql("COMMENT ON TYPE saved IS 'usable'", &[])
        .unwrap();
    drop((engine, original));
    assert_eq!(memory.used(), 0);
}

#[rstest::rstest]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn native_foreign_restore_preserves_the_paired_root_and_stored_rows_on_admission_failure(
    #[case] provider: Provider,
) {
    let directory = tempfile::tempdir().unwrap();
    let engine = provider.open(&directory.path().join("foreign.db"));
    engine.sql("CREATE SERVER remote FOREIGN DATA WRAPPER memory_fdw; CREATE FOREIGN TABLE saved(id integer DEFAULT 7) SERVER remote; CREATE ROLE reader; GRANT SELECT(id) ON saved TO reader", &[]).unwrap();
    let memory = MemoryBudget::new(1 << 20);
    let current = engine.durable.foreign_catalog.read().clone();
    let mut next = uqa_execution::catalog::foreign::ForeignCatalog::with_budget(&memory);
    for (relation, table) in current.tables() {
        next.insert(
            relation.clone(),
            table.clone(),
            current.security()[relation].clone(),
        )
        .unwrap();
    }
    *engine.durable.foreign_catalog.write() = next;
    let original = engine.durable.foreign_catalog.snapshot();
    let catalog = engine.storage.catalog.as_ref().unwrap();
    let stored = format!("{:?}", catalog.load_foreign_tables().unwrap());
    let baseline = memory.used();
    let occupied = memory.reserve(memory.available()).unwrap();
    let error = engine
        .restore_foreign_registries_from_catalog(catalog.as_ref(), CatalogRestoreMode::LoadOnly)
        .unwrap_err();
    assert_eq!(
        uqa_sql::catalog::errors::storage_error("foreign restore", &error).sqlstate(),
        Some("53200"),
        "{provider:?}: {error}"
    );
    assert!(Arc::ptr_eq(
        &original,
        &engine.durable.foreign_catalog.snapshot()
    ));
    assert_eq!(
        format!("{:?}", catalog.load_foreign_tables().unwrap()),
        stored
    );
    drop(occupied);
    assert_eq!(memory.used(), baseline);
    engine
        .restore_foreign_registries_from_catalog(catalog.as_ref(), CatalogRestoreMode::LoadOnly)
        .unwrap();
    let relation = uqa_core::RelationIdentity::new("public", "saved");
    assert_eq!(
        engine.durable.foreign_catalog.tables()[&relation]
            .schema_json()
            .unwrap(),
        original.tables()[&relation].schema_json().unwrap()
    );
    assert_eq!(
        &engine.durable.foreign_catalog.security()[&relation],
        &original.security()[&relation]
    );
    engine
        .sql(
            "ALTER FOREIGN TABLE saved RENAME TO usable; DROP FOREIGN TABLE usable",
            &[],
        )
        .unwrap();
    drop((engine, original));
    assert_eq!(memory.used(), 0);
}
