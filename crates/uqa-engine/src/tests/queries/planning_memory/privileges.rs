//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::{memory::ProductionControl, CancellationToken};
use uqa_sql::semantics::view_privileges::ViewPrivilegeCatalog;

#[rstest::rstest]
#[case::memory(Provider::Memory)]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn native_view_privilege_analysis_admits_lineage_and_recovers(#[case] provider: Provider) {
    let directory = tempfile::tempdir().unwrap();
    let engine = provider.open(&directory.path().join("privilege-lineage.db"));
    let UnifiedPlan::Query(mut query) = UnifiedPlan::lower(
        uqa_sql::compile("SELECT * FROM (SELECT 1 AS label) AS derived")
            .unwrap()
            .remove(0),
    ) else {
        panic!("query");
    };
    let uqa_sql::plan::RelationalPlan::QueryBlock(block) = &mut query.root else {
        panic!("block");
    };
    let uqa_sql::plan::SourcePlan::Subquery { body, .. } = block.from.as_mut().unwrap() else {
        panic!("source");
    };
    let uqa_sql::plan::RelationalPlan::QueryBlock(inner) = &mut body.root else {
        panic!("inner");
    };
    inner.projections[0].alias = Some("wide_column_".repeat(256 * 1024));
    let resources = engine.query_retention_control().unwrap();
    let memory = resources.memory();
    let invoking = CancellationToken::new();
    let control = ProductionControl::new(memory, resources.cancellation(), &invoking);
    let before = memory.used();
    let occupied = memory.reserve(memory.available() - 16 * 1024).unwrap();
    let allocation = allocation_counter::measure(|| {
        assert_eq!(
            ViewPrivilegeCatalog::ensure_query_select_with_control(&engine, &query, &control)
                .unwrap_err()
                .sqlstate(),
            Some("53200"),
            "{provider:?}"
        );
    });
    assert!(
        allocation.bytes_total < 256 * 1024,
        "{provider:?}: {allocation:?}"
    );
    assert_eq!(memory.used(), before + occupied.bytes());
    drop(occupied);
    for cancellation in [resources.cancellation(), &invoking] {
        cancellation.cancel();
        assert_eq!(
            ViewPrivilegeCatalog::ensure_query_select_with_control(&engine, &query, &control)
                .unwrap_err()
                .sqlstate(),
            Some("57014")
        );
        assert_eq!(memory.used(), before);
        cancellation.reset();
    }
    ViewPrivilegeCatalog::ensure_query_select_with_control(&engine, &query, &control).unwrap();
    assert_eq!(memory.used(), before);
    engine.close().unwrap();
    drop(engine);
    assert_eq!(memory.used(), 0);
}
