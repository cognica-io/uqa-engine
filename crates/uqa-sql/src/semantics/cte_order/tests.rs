//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::plan::UnifiedPlan;
use uqa_core::{memory::MemoryBudget, CancellationToken};

fn lower_sql(sql: &str) -> UnifiedPlan {
    UnifiedPlan::lower(crate::compile(sql).unwrap().remove(0))
}

fn order(
    plan: &UnifiedPlan,
    control: ProductionControl<'_>,
) -> Result<Produced<Vec<usize>>, SQLError> {
    match plan {
        UnifiedPlan::Query(query) => reachable_query_cte_order_with_control(query, control),
        UnifiedPlan::Command(command) => reachable_command_cte_order_with_control(command, control),
    }
}

#[test]
fn reachable_order_respects_lexical_names_dependencies_and_mandatory_writes() {
    for (sql, expected) in [
        ("WITH unused AS (SELECT random()) SELECT 7", vec![]),
        ("WITH a AS (VALUES(1)),b AS (VALUES(2)) SELECT * FROM b,a", vec![0,1]),
        ("WITH RECURSIVE a AS (SELECT * FROM b),b AS (VALUES(1)),unused AS (VALUES(2)) SELECT * FROM a", vec![1,0]),
        ("WITH RECURSIVE a(n) AS (VALUES(1) UNION ALL SELECT n+1 FROM a WHERE n<2) SELECT * FROM a", vec![0]),
        ("WITH \"quoted\"\"name\" AS (VALUES(1)) SELECT * FROM \"quoted\"\"name\"", vec![0]),
        ("WITH a AS (VALUES(1)) SELECT * FROM public.a", vec![]),
        ("WITH a AS (VALUES(1)) SELECT * FROM (WITH a AS (VALUES(2)) SELECT * FROM a) s", vec![]),
        ("WITH a AS (VALUES(1)) SELECT * FROM (WITH a AS (SELECT * FROM a) SELECT * FROM a) s", vec![0]),
        ("WITH a AS (VALUES(1)) SELECT * FROM (WITH RECURSIVE a AS (VALUES(2)) SELECT * FROM a) s", vec![]),
        ("WITH a AS (VALUES(1)),b AS (INSERT INTO target SELECT * FROM a) SELECT 7", vec![0,1]),
        ("WITH a AS (VALUES(1)),b AS (INSERT INTO target SELECT * FROM a) UPDATE target SET n=7", vec![0,1]),
        ("WITH a AS (VALUES(1)) INSERT INTO target WITH a AS (VALUES(2)) SELECT * FROM a", vec![]),
        ("WITH a AS (VALUES(1)) INSERT INTO target WITH a AS (SELECT * FROM a) SELECT * FROM a", vec![0]),
        ("WITH a AS (VALUES(1)) UPDATE target SET n=(SELECT * FROM a)", vec![0]),
        ("WITH a AS (VALUES(1)) DELETE FROM target USING a WHERE target.n=a.column1", vec![0]),
        ("WITH a AS (VALUES(1)) MERGE INTO target USING a ON true WHEN MATCHED THEN DELETE", vec![0]),
    ] {
        let plan = lower_sql(sql);
        let result = order(&plan, ProductionControl::uncontrolled()).unwrap();
        assert_eq!(result.as_slice(), &expected, "{sql}");
        let (definitions, reachable) = match &plan {
            UnifiedPlan::Query(query) => (query.ctes.as_slice(), crate::semantics::reachable_plan_cte_names(query)),
            UnifiedPlan::Command(command) => (command.ctes(), crate::semantics::reachable_command_cte_names(command)),
        };
        let selected = result.as_slice().iter().map(|index| definitions[*index].name.clone()).collect::<std::collections::BTreeSet<_>>();
        assert_eq!(selected, reachable, "{sql}");
    }
}

#[test]
fn controlled_order_releases_rejected_scratch_and_checks_both_cancellation_owners() {
    let plan = lower_sql("WITH RECURSIVE a AS (SELECT * FROM b),b AS (VALUES(1)) SELECT * FROM a");
    let original = CancellationToken::new();
    let invoking = CancellationToken::new();
    for allowance in [0, 16, 64] {
        let memory = MemoryBudget::new(allowance);
        let result = order(&plan, ProductionControl::new(&memory, &original, &invoking));
        assert_eq!(result.unwrap_err().sqlstate(), Some("53200"));
        assert_eq!(memory.used(), 0);
    }
    let memory = MemoryBudget::new(4096);
    let control = ProductionControl::new(&memory, &original, &invoking);
    let result = order(&plan, control).unwrap();
    assert_eq!(result.as_slice(), &[1, 0]);
    assert!(memory.used() >= 2 * size_of::<usize>());
    drop(result);
    assert_eq!(memory.used(), 0);
    for token in [&original, &invoking] {
        token.cancel();
        assert_eq!(order(&plan, control).unwrap_err().sqlstate(), Some("57014"));
        assert_eq!(memory.used(), 0);
        token.reset();
    }
    let empty = lower_sql("SELECT 1");
    let memory = MemoryBudget::new(0);
    assert!(order(
        &empty,
        ProductionControl::new(&memory, &original, &invoking)
    )
    .unwrap()
    .is_empty());
}

#[test]
fn mutual_recursion_reports_the_native_sqlstate_without_retaining_scratch() {
    let plan =
        lower_sql("WITH RECURSIVE a AS (SELECT * FROM b),b AS (SELECT * FROM a) SELECT * FROM a");
    let original = CancellationToken::new();
    let invoking = CancellationToken::new();
    let memory = MemoryBudget::new(4096);
    let error = order(&plan, ProductionControl::new(&memory, &original, &invoking)).unwrap_err();
    assert_eq!(error.sqlstate(), Some("0A000"));
    assert_eq!(memory.used(), 0);
}
