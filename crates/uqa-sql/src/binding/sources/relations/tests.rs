//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn failed_view_binding_retires_recursion_keys_and_restores_deferred_ctes() {
    use crate::binding::fixture::{empty_binding_context, NoRoutines};
    use crate::catalog::analysis::ViewDefinitionRef;
    use crate::plan::{RelationalPlan, UnifiedPlan};
    use uqa_core::{
        memory::{MemoryBudget, ProductionControl},
        CancellationToken,
    };
    let UnifiedPlan::Query(missing) =
        UnifiedPlan::lower(crate::compile("SELECT * FROM absent").unwrap().remove(0))
    else {
        panic!("query")
    };
    let UnifiedPlan::Query(mut deferred) = UnifiedPlan::lower(
        crate::compile("WITH c AS (SELECT 7 AS n) SELECT * FROM c")
            .unwrap()
            .remove(0),
    ) else {
        panic!("query")
    };
    let mut context = empty_binding_context();
    context
        .deferred_ctes
        .insert("c".into(), deferred.ctes.remove(0).into());
    let memory = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&memory, &token, &token);
    let mut scope = SchemaScope::from_context_with_control(
        context.copy_with_control(&control).unwrap(),
        &control,
    )
    .unwrap();
    let baseline = memory.used();
    for _ in 0..2 {
        let error = scope
            .bind_view_source(
                &NoRoutines,
                ViewDefinitionRef {
                    query: &missing,
                    output_columns: None,
                    materialized: false,
                    materialized_column_types: &[],
                    materialized_column_collations: None,
                },
                "bad_view",
                "v",
                &[],
                &[],
                None,
            )
            .unwrap_err();
        assert_eq!(error.sqlstate(), Some("42P01"));
        assert!(scope.visiting_views.is_empty());
        assert_eq!(memory.used(), baseline);
    }
    let RelationalPlan::QueryBlock(block) = &mut deferred.root else {
        panic!("block")
    };
    let source = block.from.as_mut().unwrap();
    let SourcePlan::Table { column_aliases, .. } = source else {
        panic!("table")
    };
    column_aliases.push("x".repeat(1 << 20));
    let occupied = memory.reserve(memory.available() - (32 << 10)).unwrap();
    assert_eq!(
        scope
            .bind_relation_source(&NoRoutines, source, &[], None)
            .unwrap_err()
            .sqlstate(),
        Some("53200")
    );
    assert!(scope.deferred_ctes.contains_key("c"));
    assert!(scope.ctes.is_empty());
    assert_eq!(memory.used(), baseline + occupied.bytes());
    drop(occupied);
    let SourcePlan::Table { column_aliases, .. } = source else {
        panic!("table")
    };
    column_aliases.clear();
    let schema = scope
        .bind_relation_source(&NoRoutines, source, &[], None)
        .unwrap();
    assert_eq!(schema.columns(), &["n"]);
    assert!(scope.deferred_ctes.contains_key("c"));
    assert!(scope.ctes.is_empty());
    drop((schema, scope));
    assert_eq!(memory.used(), 0);
}
