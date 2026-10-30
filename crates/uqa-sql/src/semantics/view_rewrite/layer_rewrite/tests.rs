//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

fn query(sql: &str) -> Box<QueryPlan> {
    let crate::plan::UnifiedPlan::Query(query) =
        crate::plan::UnifiedPlan::lower(crate::compile(sql).unwrap().remove(0))
    else {
        panic!("query")
    };
    query
}

#[test]
fn qualifier_discovery_borrows_large_payloads_and_nested_reference_names() {
    let mut query = query("SELECT 'payload', outer_scope.a FROM items AS local_scope");
    let RelationalPlan::QueryBlock(block) = &mut query.root else {
        panic!("block")
    };
    block.projections[0].expr = ScalarExpr::Literal(uqa_core::Value::Str("x".repeat(4 << 20)));
    let observed = allocation_counter::measure(|| {
        assert_eq!(fresh_query_qualifier(&query), "__uqa_view_subquery_local_0");
    });
    assert!(observed.bytes_total < 64 << 10, "{observed:?}");
    let query = self::query("WITH __uqa_view_subquery_local_0 AS (SELECT (SELECT __uqa_view_subquery_local_2.a) AS x) SELECT __uqa_view_subquery_local_1.a FROM __uqa_view_subquery_local_0 AS __uqa_view_subquery_local_3");
    assert_eq!(fresh_query_qualifier(&query), "__uqa_view_subquery_local_4");
}

#[test]
fn qualifier_selection_admits_indices_without_copying_unrelated_names() {
    use uqa_core::{
        memory::{MemoryBudget, ProductionControl},
        CancellationToken,
    };
    let mut input = query("SELECT other.id FROM items AS source");
    let RelationalPlan::QueryBlock(block) = &mut input.root else {
        panic!("block")
    };
    let ScalarExpr::QualifiedColumn { qualifier, .. } = &mut block.projections[0].expr else {
        panic!("column")
    };
    *qualifier = "x".repeat(4 << 20);
    let SourcePlan::Table { alias, .. } = block.from.as_mut().unwrap() else {
        panic!("table")
    };
    *alias = Some("y".repeat(4 << 20));
    let budget = MemoryBudget::new(64);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let mut name = None;
    let observed =
        allocation_counter::measure(|| name = Some(qualifiers::fresh(&input, &control).unwrap()));
    let name = name.unwrap();
    assert_eq!(&**name, "__uqa_view_subquery_local_0");
    assert_eq!(observed.bytes_current, budget.used() as i64);
    assert!(observed.bytes_total <= 64, "{observed:?}");
    drop(name);
    assert_eq!(budget.used(), 0);
    for sql in [
        "SELECT __uqa_view_subquery_local_00.id, __uqa_view_subquery_local_01.id",
        "SELECT \"__uqa_view_subquery_local_+0\".id, __uqa_view_subquery_local_184467440737095516160.id",
    ] { assert_eq!(fresh_query_qualifier(&query(sql)), "__uqa_view_subquery_local_0"); }
}

#[test]
fn qualifier_selection_releases_partial_indices_and_obeys_both_cancellations() {
    use uqa_core::{
        memory::{MemoryBudget, ProductionControl},
        CancellationToken,
    };
    let sql = (0..128)
        .map(|index| format!("__uqa_view_subquery_local_{index}.id"))
        .collect::<Vec<_>>()
        .join(",");
    let input = query(&format!("SELECT {sql}"));
    for limit in [16, 128, 1024, 16384] {
        let budget = MemoryBudget::new(limit);
        let statement = CancellationToken::new();
        let session = CancellationToken::new();
        let control = ProductionControl::new(&budget, &statement, &session);
        let sibling = control.copy_text("alive").unwrap();
        let before = budget.used();
        match qualifiers::fresh(&input, &control) {
            Ok(name) => assert_eq!(&**name, "__uqa_view_subquery_local_128"),
            Err(error) => assert_eq!(error.sqlstate(), Some("53200")),
        }
        assert_eq!(budget.used(), before);
        drop(sibling);
        assert_eq!(budget.used(), 0);
    }
    for statement_cancelled in [true, false] {
        let budget = MemoryBudget::new(1 << 20);
        let statement = CancellationToken::new();
        let session = CancellationToken::new();
        let control = ProductionControl::new(&budget, &statement, &session);
        if statement_cancelled {
            statement.cancel();
        } else {
            session.cancel();
        }
        let observed = allocation_counter::measure(|| {
            assert_eq!(
                qualifiers::fresh(&input, &control).unwrap_err().sqlstate(),
                Some("57014")
            );
        });
        assert!(observed.bytes_total < 1024, "{observed:?}");
        assert_eq!(budget.used(), 0);
    }
}

#[test]
fn readonly_query_visitation_stops_on_nested_cte_errors() {
    let input = query("WITH changed AS (UPDATE items SET id = (SELECT 3) WHERE id = 1 RETURNING id) SELECT id FROM changed WHERE id = 2");
    let crate::plan::CtePlanBody::Command(command) = &input.ctes[0].body else {
        panic!("command CTE")
    };
    let crate::plan::CommandPlan::Update(update) = command.as_ref() else {
        panic!("UPDATE")
    };
    let RelationalPlan::QueryBlock(scalar) = &update.subqueries[0].root else {
        panic!("scalar query")
    };
    let RelationalPlan::QueryBlock(outer) = &input.root else {
        panic!("outer query")
    };
    let expected = [
        std::ptr::from_ref(&update.assignments[0].value),
        std::ptr::from_ref(update.predicate.as_ref().unwrap()),
        std::ptr::from_ref(&update.returning[0].expr),
        std::ptr::from_ref(&scalar.projections[0].expr),
        std::ptr::from_ref(outer.r#where.as_ref().unwrap()),
        std::ptr::from_ref(&outer.projections[0].expr),
    ];
    let mut complete = Vec::new();
    input.visit_scalar_expressions(&mut |expression| complete.push(std::ptr::from_ref(expression)));
    assert_eq!(complete, expected);
    for stop in 0..expected.len() {
        let mut actual = Vec::new();
        let error = input
            .try_visit_scalar_expressions(&mut |expression| {
                actual.push(std::ptr::from_ref(expression));
                if actual.len() == stop + 1 {
                    Err(stop)
                } else {
                    Ok(())
                }
            })
            .unwrap_err();
        assert_eq!(error, stop);
        assert_eq!(actual, expected[..=stop]);
    }
}
