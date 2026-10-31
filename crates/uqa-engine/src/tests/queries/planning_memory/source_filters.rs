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

#[rstest::rstest]
#[case::memory(Provider::Memory)]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn native_predicate_placement_rejects_payloads_before_copying(#[case] provider: Provider) {
    use uqa_execution::query::sources::context::SourcePlanning;
    let directory = tempfile::tempdir().unwrap();
    let engine = provider.open(&directory.path().join("predicate-placement.db"));
    let resources = engine.query_retention_control().unwrap();
    let memory = resources.memory();
    let scope = crate::capabilities::query_scope::new_for_statement(&engine, None).unwrap();
    for placed in [true, false] {
        let sql = format!(
            "SELECT v.id FROM (VALUES(1)) AS v(id) WHERE {}length('{}'::text)>0",
            if placed { "v.id+" } else { "" },
            "x".repeat(4 << 20)
        );
        let UnifiedPlan::Query(query) =
            UnifiedPlan::lower(uqa_sql::compile(&sql).unwrap().remove(0))
        else {
            panic!("query")
        };
        let uqa_sql::plan::RelationalPlan::QueryBlock(block) = &query.root else {
            panic!("block")
        };
        let source = block.from.as_ref().unwrap();
        let baseline = memory.used();
        let occupied = memory.reserve(memory.available() - (64 << 10)).unwrap();
        let mut state = None;
        let allocation = allocation_counter::measure(|| {
            state = if placed {
                engine.qualifier_filters(block, source, &scope).err()
            } else {
                engine.residual_filter(block, source, None, &scope).err()
            };
        });
        assert_eq!(memory.used(), baseline + occupied.bytes());
        assert!(
            state.as_ref().and_then(uqa_sql::SQLError::sqlstate) == Some("53200")
                && allocation.bytes_total < 256 << 10,
            "{provider:?}, placed={placed}: {state:?}; {allocation:?}"
        );
        drop(occupied);
        assert_eq!(memory.used(), baseline);
        resources.cancellation().cancel();
        let cancelled = if placed {
            engine.qualifier_filters(block, source, &scope).err()
        } else {
            engine.residual_filter(block, source, None, &scope).err()
        };
        assert_eq!(cancelled.unwrap().sqlstate(), Some("57014"));
        assert_eq!(memory.used(), baseline);
        resources.cancellation().reset();
        if placed {
            let result = engine
                .qualifier_filters(block, source, &scope)
                .unwrap()
                .unwrap();
            assert!(memory.used() > baseline + (4 << 20));
            assert_eq!(result.get("v").unwrap().len(), 1);
            drop(result);
        } else {
            let result = engine
                .residual_filter(block, source, None, &scope)
                .unwrap()
                .unwrap();
            assert!(memory.used() > baseline + (4 << 20));
            assert_eq!(&*result, block.r#where.as_ref().unwrap());
            drop(result);
        }
        assert_eq!(memory.used(), baseline);
    }
}

#[rstest::rstest]
#[case::memory(Provider::Memory)]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn native_physical_explain_admits_placed_predicates(#[case] provider: Provider) {
    use uqa_execution::statement::context::PhysicalExplainPlanning;
    let directory = tempfile::tempdir().unwrap();
    let engine = provider.open(&directory.path().join("physical-explain-predicates.db"));
    let resources = engine.query_retention_control().unwrap();
    let memory = resources.memory();
    let invoking = uqa_core::CancellationToken::new();
    let control =
        uqa_core::memory::ProductionControl::new(memory, resources.cancellation(), &invoking);
    for cte in [false, true] {
        let sql = format!(
            "{}SELECT v.id FROM {} v WHERE v.id+length('{}'::text)>0",
            if cte {
                "WITH input(id) AS (VALUES(1)) "
            } else {
                ""
            },
            if cte { "input" } else { "(SELECT 1 AS id)" },
            "x".repeat(4 << 20)
        );
        let plan = UnifiedPlan::lower(uqa_sql::compile(&sql).unwrap().remove(0));
        let baseline = memory.used();
        let occupied = memory.reserve(memory.available() - (64 << 10)).unwrap();
        let mut error = None;
        let allocation = allocation_counter::measure(|| {
            error = engine
                .physical_explain_plan(&plan, &[], false, &control)
                .err();
        });
        assert_eq!(memory.used(), baseline + occupied.bytes());
        assert!(
            error.as_ref().and_then(uqa_sql::SQLError::sqlstate) == Some("53200")
                && allocation.bytes_total < 256 << 10,
            "{provider:?}, cte={cte}: {error:?}; {allocation:?}"
        );
        drop(occupied);
        for token in [resources.cancellation(), &invoking] {
            token.cancel();
            assert_eq!(
                engine
                    .physical_explain_plan(&plan, &[], false, &control)
                    .unwrap_err()
                    .sqlstate(),
                Some("57014")
            );
            assert_eq!(memory.used(), baseline);
            token.reset();
        }
        let output = engine
            .physical_explain_plan(&plan, &[], false, &control)
            .unwrap();
        drop(output);
        assert_eq!(memory.used(), baseline);
    }
}
#[rstest::rstest]
#[case::memory(Provider::Memory)]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn native_derived_filter_specialization_admits_query_copies(#[case] provider: Provider) {
    use uqa_execution::query::cte::context::QueryOutputRewriter;
    let directory = tempfile::tempdir().unwrap();
    let engine = provider.open(&directory.path().join("derived-filter-owners.db"));
    let query = |payload: &str| {
        let UnifiedPlan::Query(query) = UnifiedPlan::lower(
            uqa_sql::compile(&format!(
                "SELECT v.id AS k,'{payload}'::text AS payload FROM(VALUES(1),(2)) v(id)"
            ))
            .unwrap()
            .remove(0),
        ) else {
            panic!("query")
        };
        query
    };
    let predicate = uqa_sql::ScalarExpr::Binary {
        op: uqa_sql::ast::BinaryOp::Equal,
        lhs: Box::new(uqa_sql::ScalarExpr::qualified_column("q", "k")),
        rhs: Box::new(uqa_sql::ScalarExpr::Literal(Value::Int(2))),
    };
    drop(
        engine
            .push_derived_output_filter(&query("small"), "q", &predicate, None)
            .unwrap(),
    );
    let query = query(&"x".repeat(4 << 20));
    let resources = engine.query_retention_control().unwrap();
    let memory = resources.memory();
    for recursive in [false, true] {
        let invoke = || {
            if recursive {
                engine.push_output_filter(&query, "q", &predicate, None)
            } else {
                engine.push_derived_output_filter(&query, "q", &predicate, None)
            }
        };
        let baseline = memory.used();
        let occupied = memory.reserve(memory.available() - (64 << 10)).unwrap();
        let mut error = None;
        let allocation = allocation_counter::measure(|| {
            error = invoke().err();
        });
        assert_eq!(memory.used(), baseline + occupied.bytes());
        assert!(
            error.as_ref().and_then(uqa_sql::SQLError::sqlstate) == Some("53200")
                && allocation.bytes_total < 256 << 10,
            "{provider:?}, recursive={recursive}: {error:?}; {allocation:?}"
        );
        drop(occupied);
        resources.cancellation().cancel();
        assert_eq!(invoke().unwrap_err().sqlstate(), Some("57014"));
        assert_eq!(memory.used(), baseline);
        resources.cancellation().reset();
        let specialized = invoke().unwrap().unwrap();
        assert!(specialized.reserved_bytes() > 0);
        assert!(memory.used() > baseline);
        drop(specialized);
        assert_eq!(memory.used(), baseline);
    }
}

#[rstest::rstest]
#[case::memory(Provider::Memory)]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn native_explain_optimizer_admits_its_borrowed_statement(#[case] provider: Provider) {
    use uqa_execution::statement::context::PhysicalExplainPlanning;
    let directory = tempfile::tempdir().unwrap();
    let engine = provider.open(&directory.path().join("explain-optimizer-owner.db"));
    let query = |payload: &str| {
        UnifiedPlan::lower(
            uqa_sql::compile(&format!("SELECT '{payload}'::text AS payload"))
                .unwrap()
                .remove(0),
        )
    };
    drop(
        engine
            .plan_explained_statement((&query("small")).into())
            .unwrap(),
    );
    let input = query(&"x".repeat(4 << 20));
    let resources = engine.query_retention_control().unwrap();
    let memory = resources.memory();
    let baseline = memory.used();
    let occupied = memory.reserve(memory.available() - (64 << 10)).unwrap();
    let mut error = None;
    let allocation = allocation_counter::measure(|| {
        error = engine.plan_explained_statement((&input).into()).err();
    });
    assert_eq!(memory.used(), baseline + occupied.bytes());
    assert!(
        error.as_ref().and_then(uqa_sql::SQLError::sqlstate) == Some("53200")
            && allocation.bytes_total < 256 << 10,
        "{provider:?}: {error:?}; {allocation:?}"
    );
    drop(occupied);
    resources.cancellation().cancel();
    assert_eq!(
        engine
            .plan_explained_statement((&input).into())
            .unwrap_err()
            .sqlstate(),
        Some("57014")
    );
    assert_eq!(memory.used(), baseline);
    resources.cancellation().reset();
    let result = engine.plan_explained_statement((&input).into()).unwrap();
    assert!(result.reserved_bytes() > 0);
    assert!(memory.used() > baseline);
    drop(result);
    assert_eq!(memory.used(), baseline);
}
#[rstest::rstest]
#[case::memory(Provider::Memory)]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn native_table_scan_admits_selected_predicates_before_copying(#[case] provider: Provider) {
    use uqa_execution::query::sources::build_join_operator_with_ctes;
    let directory = tempfile::tempdir().unwrap();
    let engine = provider.open(&directory.path().join("table-scan-owners.db"));
    engine.sql("CREATE TABLE scan_owners(id integer PRIMARY KEY, payload text); INSERT INTO scan_owners VALUES(1,'small')", &[]).unwrap();
    let sql = format!(
        "SELECT q.id,q.payload FROM scan_owners q WHERE q.payload='{}'::text",
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
        uqa_sql::plan::source_projection::QualifierFilters::from([("q".into(), vec![predicate])]);
    let source = block.from.as_ref().unwrap();
    let resources = engine.query_retention_control().unwrap();
    let memory = resources.memory();
    let context = engine.source_execution_context();
    let mut scope = crate::capabilities::query_scope::new_for_statement(&engine, None).unwrap();
    let mut warm =
        build_join_operator_with_ctes(&context, source, &[], &mut scope, None, None).unwrap();
    warm.close().unwrap();
    drop(warm);
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
    assert!(operator.next().unwrap().is_none());
    operator.close().unwrap();
    drop(operator);
    assert_eq!(memory.used(), baseline);
}
