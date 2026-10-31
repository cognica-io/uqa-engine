//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[rstest::rstest]
#[case::memory(Provider::Memory)]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn native_source_filters_reject_wide_predicates_before_copying(#[case] provider: Provider) {
    use uqa_execution::query::sources::build_join_operator_with_ctes;
    let directory = tempfile::tempdir().unwrap();
    let engine = provider.open(&directory.path().join("source-filter-owners.db"));
    let sql = format!(
        "SELECT v.id FROM (VALUES(1)) AS v(id) WHERE length('{}'::text)>0",
        "x".repeat(4 << 20)
    );
    let UnifiedPlan::Query(mut query) =
        UnifiedPlan::lower(uqa_sql::compile(&sql).unwrap().remove(0))
    else {
        panic!("query")
    };
    let uqa_sql::plan::RelationalPlan::QueryBlock(block) = &mut query.root else {
        panic!("block")
    };
    let predicate = block.r#where.take().unwrap();
    let filters =
        uqa_sql::plan::source_projection::QualifierFilters::from([("v".into(), vec![predicate])]);
    let source = block.from.as_ref().unwrap();
    let resources = engine.query_retention_control().unwrap();
    let memory = resources.memory();
    let context = engine.source_execution_context();
    let mut scope = crate::capabilities::query_scope::new_for_statement(&engine, None).unwrap();
    let baseline = memory.used();
    let occupied = memory.reserve(memory.available() - (64 << 10)).unwrap();
    let mut result = None;
    let allocation = allocation_counter::measure(|| {
        result = Some(build_join_operator_with_ctes(
            &context,
            source,
            &[],
            &mut scope,
            None,
            Some(&filters),
        ));
    });
    let result = result.unwrap();
    let state = result
        .as_ref()
        .err()
        .and_then(uqa_sql::SQLError::sqlstate)
        .map(str::to_owned);
    if let Ok(mut operator) = result {
        operator.close().unwrap();
    }
    assert_eq!(memory.used(), baseline + occupied.bytes());
    assert!(
        state.as_deref() == Some("53200") && allocation.bytes_total < 256 << 10,
        "{provider:?}: {state:?}; {allocation:?}"
    );
    drop(occupied);
    assert_eq!(memory.used(), baseline);
    resources.cancellation().cancel();
    let result =
        build_join_operator_with_ctes(&context, source, &[], &mut scope, None, Some(&filters));
    assert_eq!(result.err().unwrap().sqlstate(), Some("57014"));
    assert_eq!(memory.used(), baseline);
    resources.cancellation().reset();
    let mut operator =
        build_join_operator_with_ctes(&context, source, &[], &mut scope, None, Some(&filters))
            .unwrap();
    assert!(memory.used() > baseline + (4 << 20));
    operator.open().unwrap();
    let batch = operator.next().unwrap().unwrap();
    assert_eq!(batch.rows.len(), 1);
    assert!(operator.next().unwrap().is_none());
    operator.close().unwrap();
    drop(operator);
    drop(batch);
    assert_eq!(memory.used(), baseline);
}
