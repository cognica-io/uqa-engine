//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::plan::ComputePlan;

fn registered(name: &str) -> bool {
    name == "custom_measure"
}

fn public_input(
    source: Arc<Statement>,
    native: &MemoryBudget,
    work: &MemoryBudget,
    original: &CancellationToken,
    invoking: &CancellationToken,
) -> PlanInput {
    let native = native.clone();
    let original = original.clone();
    ImageDecoder::new(work, invoking, |mut reader| async move {
        let mut channel = reader.input();
        let control = ProductionControl::new(&native, &original, channel.cancellation);
        UnifiedPlan::lower_executable_from_input(&source, &registered, &control, &mut channel).await
    })
    .unwrap()
}

#[test]
fn native_statement_entry_preserves_registered_aggregates_and_original_names() {
    let native = MemoryBudget::new(32 << 20);
    let work = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    for sql in [
        "SELECT custom_measure(v) AS reject_source_alias FROM (VALUES (1)) AS reject_source_alias(v)",
        "WITH r AS (SELECT custom_measure(v) FROM (VALUES (1)) s(v)) SELECT * FROM r",
        "INSERT INTO t VALUES (1) RETURNING (SELECT custom_measure(v) FROM (VALUES (2)) s(v))",
        "SELECT reject_function() AS reject_source_alias",
    ] {
        let source = Arc::new(statement(sql));
        let control = ProductionControl::new(&native, &token, &token);
        let expected = UnifiedPlan::lower_borrowed_for_rewrite(&source, &registered, &control)
            .unwrap();
        let (actual, polls) = complete(&mut public_input(source, &native, &work, &token, &token));
        assert_eq!(json(actual.as_plan_ref()), json(expected.as_plan_ref()), "{sql}");
        if sql.starts_with("SELECT custom_measure") {
            let PlanRef::Query(query) = actual.as_plan_ref() else { panic!("query"); };
            let RelationalPlan::QueryBlock(block) = &query.root else { panic!("block"); };
            assert!(matches!(block.compute, ComputePlan::Aggregate));
        }
        assert!(polls > 1);
        assert!(actual.reserved_bytes() > 0);
        drop((actual, expected));
        assert_eq!((native.used(), work.used()), (0, 0));
    }
}

#[test]
fn native_statement_entry_preserves_nested_window_error_before_later_catalog_failure() {
    let mut child = blocks::query("SELECT row_number() OVER w WINDOW w AS (ORDER BY 1)");
    child.windows.clear();
    let mut parent = blocks::query("SELECT 1, reject_function()");
    parent.projections[0].expr = Expr::ScalarSubquery(Box::new(child));
    let source = Arc::new(Statement::Select(Box::new(parent)));
    let native = MemoryBudget::new(32 << 20);
    let work = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&native, &token, &token);
    let expected =
        UnifiedPlan::lower_borrowed_for_rewrite(&source, &Aggregates, &control).unwrap_err();
    assert!(expected.to_string().contains("window"), "{expected}");
    let actual = input(source, &native, &work, &token, &token, false)
        .read(&mut std::io::empty())
        .unwrap_err();
    assert_eq!(
        (actual.sqlstate(), actual.to_string()),
        (expected.sqlstate(), expected.to_string())
    );
    drop((actual, expected));
    assert_eq!((native.used(), work.used()), (0, 0));
}

#[test]
fn native_statement_result_keeps_completed_fields_after_source_and_input_retirement() {
    let native = MemoryBudget::new(32 << 20);
    let work = MemoryBudget::new(1 << 20);
    let original = CancellationToken::new();
    let invoking = CancellationToken::new();
    let sibling = native.reserve(37).unwrap();
    let source = Arc::new(statement("WITH q AS (SELECT row_number() OVER w n WINDOW w AS (ORDER BY 2)) SELECT custom_measure(n), (SELECT row_number() OVER w WINDOW w AS (ORDER BY 3)) FROM q"));
    let expected = UnifiedPlan::lower_with(source.as_ref().clone(), &registered);
    let mut reader = public_input(source.clone(), &native, &work, &original, &invoking);
    let (actual, _) = complete(&mut reader);
    drop((reader, source));
    original.cancel();
    invoking.cancel();
    assert_eq!(work.used(), 0);
    assert_eq!(json(actual.as_plan_ref()), json(PlanRef::from(&expected)));
    assert!(native.used() >= actual.reserved_bytes() + sibling.bytes());
    assert_eq!(allocation_counter::measure(|| drop(actual)).count_total, 0);
    assert_eq!(native.used(), sibling.bytes());
}
