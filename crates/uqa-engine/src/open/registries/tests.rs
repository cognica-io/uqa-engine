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
        admitted
            .insert(*address, text, uqa_sql::ast::RelationPersistence::Permanent)
            .unwrap();
    }
    *engine.durable.comments.write() = admitted;
    let before = engine.durable.comments.snapshot();
    let catalog = engine.storage.catalog.as_ref().unwrap();
    let occupied = memory.reserve(memory.available()).unwrap();
    engine
        .restore_engine_registries_from_catalog(catalog.as_ref(), CatalogRestoreMode::LoadOnly)
        .unwrap();
    let original = engine.durable.comments.snapshot();
    for (address, text) in before.iter() {
        assert!(std::ptr::eq(original.get(address).unwrap(), text));
    }
    drop((occupied, before));
    let key = catalog
        .metadata_with_prefix("uqa.sql.comment.v1:")
        .unwrap()
        .into_iter()
        .next()
        .unwrap()
        .0;
    catalog.set_metadata(&key, "stored replacement").unwrap();
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
    assert_eq!(engine.durable.comments.read().len(), 1);
    assert!(engine
        .durable
        .comments
        .read()
        .iter()
        .all(|(_, text)| text == "stored replacement"));
    assert!(original.iter().all(|(_, text)| text == "original 한글"));
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

#[rstest::rstest]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn native_index_restore_preserves_original_generation_and_stored_rows_on_admission_failure(
    #[case] provider: Provider,
) {
    let directory = tempfile::tempdir().unwrap();
    let engine = provider.open(&directory.path().join("indexes.db"));
    engine.sql("CREATE TABLE saved(id integer); CREATE INDEX saved_index ON saved(id); CREATE TEMP TABLE local_items(id integer); CREATE INDEX local_index ON local_items(id)", &[]).unwrap();
    let memory = MemoryBudget::new(1 << 20);
    let mut next = uqa_execution::catalog::index::registry::CatalogIndexes::with_budget(&memory);
    for row in engine.durable.catalog_indexes.read().values() {
        let relation = uqa_core::RelationIdentity::from_legacy_name(&row.table_name).unwrap();
        next.insert(
            row.clone(),
            engine.storage.tables.read()[&relation].persistence,
        )
        .unwrap();
    }
    *engine.durable.catalog_indexes.write() = next;
    let original = engine.durable.catalog_indexes.snapshot();
    let catalog = engine.storage.catalog.as_ref().unwrap();
    let stored = format!("{:?}", catalog.load_catalog_indexes().unwrap());
    let baseline = memory.used();
    let occupied = memory.reserve(memory.available()).unwrap();
    let error = engine
        .restore_catalog_indexes_from_catalog(catalog.as_ref(), CatalogRestoreMode::LoadOnly)
        .unwrap_err();
    assert_eq!(
        uqa_execution::storage_errors::storage_error("index restore", &error).sqlstate(),
        Some("53200"),
        "{provider:?}: {error}"
    );
    assert!(Arc::ptr_eq(
        &original,
        &engine.durable.catalog_indexes.snapshot()
    ));
    assert_eq!(
        format!("{:?}", catalog.load_catalog_indexes().unwrap()),
        stored
    );
    drop(occupied);
    assert_eq!(memory.used(), baseline);
    engine
        .restore_catalog_indexes_from_catalog(catalog.as_ref(), CatalogRestoreMode::LoadOnly)
        .unwrap();
    let restored = engine.durable.catalog_indexes.snapshot();
    assert_eq!(restored.len(), original.len());
    for (key, row) in original.iter() {
        assert_eq!(
            format!("{:?}", restored.get(key).unwrap()),
            format!("{row:?}")
        );
        assert_eq!(restored.is_durable(key), original.is_durable(key));
        if !original.is_durable(key) {
            assert!(std::ptr::eq(row, restored.get(key).unwrap()));
        }
    }
    engine
        .sql(
            "ALTER INDEX saved_index RENAME TO usable; DROP INDEX local_index",
            &[],
        )
        .unwrap();
    drop((engine, restored, original));
    assert_eq!(memory.used(), 0);
}
