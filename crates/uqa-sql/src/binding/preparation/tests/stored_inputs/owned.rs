//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::{
    memory::{MemoryBudget, ProductionControl},
    CancellationToken,
};

fn descriptors(query: &mut QueryPlan) -> Vec<RowSchema> {
    fn inputs(source: &crate::plan::AnalyzedSource, output: &mut Vec<RowSchema>) {
        output.push(source.schema.clone());
        if let Some(children) = &source.children {
            inputs(&children.0, output);
            inputs(&children.1, output);
        }
    }
    let mut output = Vec::new();
    query.visit_queries_mut(&mut |query| {
        output.push(query.analyzed_output.clone().expect("query output"));
        if let Some(source) = &query.analyzed_source {
            inputs(source, &mut output);
        }
    });
    output
}

#[test]
fn owned_layouts_preserve_nested_namespace_and_lazy_expression_observations() {
    for sql in [
        "SELECT s.k,$1 AS p,outer_scope.x FROM (SELECT x AS k,10/x AS z FROM (VALUES(0),(2)) v(x)) s WHERE s.k=(SELECT 2)",
        "WITH c(x) AS (SELECT 2::smallint) SELECT s.k FROM (SELECT x AS k FROM c) s CROSS JOIN (SELECT 1 AS x) c WHERE s.k>0",
        "SELECT x FROM (SELECT NULL AS x UNION ALL SELECT 2::smallint) s WHERE x>0",
        "SELECT x, count(*) FROM (VALUES(1),(2)) v(x) GROUP BY GROUPING SETS ((x),())",
    ] {
        let UnifiedPlan::Query(mut expected) = UnifiedPlan::lower(crate::compile(sql).unwrap().remove(0)) else { panic!("query") };
        let budget = MemoryBudget::new(16 << 20);
        let token = CancellationToken::new();
        let control = ProductionControl::new(&budget, &token, &token);
        let input = expected.copy_for_rewrite(&control).unwrap();
        let outer = RowSchema::with_qualified_types("outer_scope", vec!["x".into()], vec![Some(ColumnType::BigInteger)]);
        let params = [crate::SQLParam::typed_scalar(uqa_core::Value::Int(9), ColumnType::BigInteger)];
        let binding = crate::binding::fixture::empty_binding_context();
        restore_query_layouts(&NoRoutines, &mut expected, &params, &binding, Some(&outer)).unwrap();
        let actual = restore_query_layouts_owned(&NoRoutines, input, &params, binding.copy_with_control(&control).unwrap(), Some(&outer), &control).unwrap();
        let (mut actual, memory) = actual.finish(&control).unwrap().into_parts();
        assert_eq!(serde_json::to_value(&actual).unwrap(), serde_json::to_value(&expected).unwrap(), "{sql}");
        assert_eq!(descriptors(&mut actual), descriptors(&mut expected), "{sql}");
        drop(actual);
        drop(memory);
        assert_eq!(budget.used(), 0, "{sql}");
    }
}

#[test]
fn layout_admission_and_both_cancellations_release_the_consumed_destination() {
    for failure in 0..3 {
        let budget = MemoryBudget::new(1 << 20);
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        let control = ProductionControl::new(&budget, &original, &invoking);
        let UnifiedPlan::Query(query) = UnifiedPlan::lower(
            crate::compile("SELECT x FROM (VALUES(1)) v(x)")
                .unwrap()
                .remove(0),
        ) else {
            panic!("query")
        };
        let query = query.copy_for_rewrite(&control).unwrap();
        let binding = crate::binding::fixture::empty_binding_context();
        let binding = binding.copy_with_control(&control).unwrap();
        let occupied = (failure == 0).then(|| budget.reserve(budget.available()).unwrap());
        if failure == 1 {
            original.cancel();
        }
        if failure == 2 {
            invoking.cancel();
        }
        let error = restore_query_layouts_owned(&NoRoutines, query, &[], binding, None, &control)
            .unwrap_err();
        assert_eq!(
            error.sqlstate(),
            Some(if failure == 0 { "53200" } else { "57014" })
        );
        assert_eq!(
            budget.used(),
            occupied
                .as_ref()
                .map_or(0, uqa_core::memory::MemoryReservation::bytes)
        );
        drop(occupied);
        assert_eq!(budget.used(), 0);
    }
}
