//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::{memory::MemoryBudget, CancellationToken};
use uqa_sql::{
    plan::{AnalyzedSource, QueryPlan, UnifiedPlan},
    ColumnType,
};

fn query(sql: &str) -> QueryPlan {
    let UnifiedPlan::Query(query) = UnifiedPlan::lower(uqa_sql::compile(sql).unwrap().remove(0))
    else {
        unreachable!()
    };
    *query
}

#[test]
fn discarded_output_storage_retires_while_positional_type_and_live_alias_survive() {
    let mut input = query("SELECT 'unused'::text AS unused, 7 AS kept");
    let RelationalPlan::QueryBlock(block) = &mut input.root else {
        unreachable!()
    };
    let ScalarExpr::Cast { expr, .. } = &mut block.projections[0].expr else {
        unreachable!()
    };
    **expr = ScalarExpr::Literal(Value::Str("discarded".repeat(8192)));
    input.analyzed_output = Some(RowSchema::with_types(
        vec!["unused".into(), "kept".into()],
        vec![Some(ColumnType::Text), Some(ColumnType::Integer)],
    ));
    let budget = MemoryBudget::new(4 * 1024 * 1024);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let input = input.copy_for_rewrite(&control).unwrap();
    let RelationalPlan::QueryBlock(block) = &input.root else {
        unreachable!()
    };
    let kept_alias = block.projections[1].alias.as_ref().unwrap().as_ptr();
    let before = budget.used();
    let mut output = None;
    let allocations = allocation_counter::measure(|| {
        output = Some(
            prune_positions(
                input,
                &|position| position == 1,
                None,
                OutputPruningContext {
                    production: control,
                    ..OutputPruningContext::new(&|_: &str| false)
                },
            )
            .unwrap(),
        );
    });
    let output = output.unwrap();
    assert_eq!(
        before as i64 + allocations.bytes_current,
        budget.used() as i64
    );
    assert_eq!(budget.used(), output.reserved_bytes());
    assert!(budget.used() < before - 64 * 1024);
    let RelationalPlan::QueryBlock(block) = &output.root else {
        unreachable!()
    };
    assert!(matches!(
        &block.projections[0].expr,
        ScalarExpr::TypedLiteral {
            value: Value::Null,
            bound_type: Some(ColumnType::Text),
            ..
        }
    ));
    assert_eq!(
        block.projections[1].alias.as_ref().unwrap().as_ptr(),
        kept_alias
    );
    drop(output);
    assert_eq!(budget.used(), 0);
}

fn star_query() -> QueryPlan {
    let mut input = query("SELECT q.*, 99 AS kept FROM data q");
    input.analyzed_source = Some(
        std::sync::Arc::new(AnalyzedSource {
            schema: RowSchema::with_qualified_types(
                "q",
                vec!["a".into(), "b".into()],
                vec![Some(ColumnType::Integer); 2],
            ),
            children: None,
        })
        .into(),
    );
    input.analyzed_output = Some(RowSchema::with_types(
        vec!["a".into(), "b".into(), "kept".into()],
        vec![Some(ColumnType::Integer); 3],
    ));
    input
}

#[test]
fn wildcard_expansion_keeps_surviving_owners_and_discarded_column_privileges() {
    let source = star_query();
    let mut expected = source.clone();
    super::super::prune_positions(
        &mut expected,
        &|position| position != 1,
        None,
        OutputPruningContext::new(&|_: &str| false),
    )
    .unwrap();
    let budget = MemoryBudget::new(4 * 1024 * 1024);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let input = source.copy_for_rewrite(&control).unwrap();
    let RelationalPlan::QueryBlock(block) = &input.root else {
        unreachable!()
    };
    let kept_alias = block.projections[1].alias.as_ref().unwrap().as_ptr();
    let before = budget.used();
    let mut output = None;
    let allocations = allocation_counter::measure(|| {
        output = Some(
            prune_positions(
                input,
                &|position| position != 1,
                None,
                OutputPruningContext {
                    production: control,
                    ..OutputPruningContext::new(&|_: &str| false)
                },
            )
            .unwrap(),
        );
    });
    let output = output.unwrap();
    assert_eq!(
        before as i64 + allocations.bytes_current,
        budget.used() as i64
    );
    assert_eq!(budget.used(), output.reserved_bytes());
    assert_eq!(
        serde_json::to_value(&*output).unwrap(),
        serde_json::to_value(expected).unwrap()
    );
    let RelationalPlan::QueryBlock(block) = &output.root else {
        unreachable!()
    };
    assert_eq!(
        block.projections[2].alias.as_ref().unwrap().as_ptr(),
        kept_alias
    );
    drop(output);
    assert_eq!(budget.used(), 0);
}

#[test]
fn failed_wildcard_destination_and_both_cancellations_release_private_query() {
    let source = star_query();
    for mode in 0..3 {
        let budget = MemoryBudget::new(4 * 1024 * 1024);
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        let control = ProductionControl::new(&budget, &original, &invoking);
        let input = source.copy_for_rewrite(&control).unwrap();
        let sibling = budget.reserve(budget.available()).unwrap();
        if mode == 1 {
            original.cancel();
        }
        if mode == 2 {
            invoking.cancel();
        }
        let error = prune_positions(
            input,
            &|_| false,
            None,
            OutputPruningContext {
                production: control,
                ..OutputPruningContext::new(&|_: &str| false)
            },
        )
        .unwrap_err();
        assert_eq!(
            error.sqlstate(),
            Some(if mode == 0 { "53200" } else { "57014" })
        );
        assert_eq!(budget.used(), sibling.bytes());
    }
}
