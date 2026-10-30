//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{
    ast::RelationPersistence,
    catalog::{
        roles::RoleIdentity, security::BoundTableSecurity, stored_view::StoredViewDefinition,
    },
    plan::{RelationalPlan, UnifiedPlan},
    ScalarExpr,
};
use uqa_core::{
    memory::{MemoryBudget, ProductionVec},
    CancellationToken, Value,
};

fn view(payload: Value) -> StoredView {
    let UnifiedPlan::Query(mut query) =
        UnifiedPlan::lower(crate::compile("SELECT 1 AS original").unwrap().remove(0))
    else {
        panic!("query")
    };
    let RelationalPlan::QueryBlock(block) = &mut query.root else {
        panic!("block")
    };
    block.projections[0].expr = ScalarExpr::Literal(payload);
    StoredView {
        security: BoundTableSecurity::owner(RoleIdentity {
            oid: 42,
            object_id: [42; 16],
        }),
        definition: StoredViewDefinition {
            object_id: [7; 16],
            catalog_oids: None,
            row_type_array_name: None,
            query: *query,
            output_columns: Some(vec!["visible".into()]),
            persistence: RelationPersistence::Permanent,
            options: vec![("security_invoker".into(), "true".into())],
            kind: StoredViewKind::View,
            materialized_rows: Vec::new(),
            materialized_column_types: Vec::new(),
            materialized_column_collations: None,
            populated: true,
        },
    }
}

fn schema(control: &ProductionControl<'_>) -> RowSchema {
    let mut names = ProductionVec::new(*control);
    names
        .push_produced(control.copy_text("visible").unwrap())
        .unwrap();
    RowSchema::new_with_control(names.finish().unwrap(), control).unwrap()
}

#[test]
fn view_target_capture_selects_metadata_and_owns_exact_query_allocations() {
    let mut definition = view(Value::Str("x".repeat(4 << 20)));
    definition
        .options
        .push(("unrelated".into(), "u".repeat(4 << 20)));
    definition.row_type_array_name = Some("a".repeat(4 << 20));
    let budget = MemoryBudget::new(32 << 20);
    let cancellation = CancellationToken::new();
    let control = ProductionControl::new(&budget, &cancellation, &cancellation);
    let schema = schema(&control);
    let before = budget.used();
    let mut result = None;
    let observed = allocation_counter::measure(|| {
        result = Some(capture("public.v", &definition, schema, &control).unwrap());
    });
    let result = result.unwrap();
    assert_eq!(budget.used() as i64, before as i64 + observed.bytes_current);
    assert!(
        observed.bytes_total < (4 << 20) + (64 << 10),
        "{observed:?}"
    );
    assert_eq!(result.canonical_name, "public.v");
    assert_eq!(result.columns, ["visible"]);
    assert_eq!(result.security.role_owner, definition.security.role_owner);
    assert!(result.security_invoker);
    assert_eq!(
        serde_json::to_value(&result.query).unwrap(),
        serde_json::to_value(&definition.query).unwrap()
    );
    drop(definition);
    assert_eq!(result.schema.public_name(0), Some("visible"));
    drop(result);
    assert_eq!(budget.used(), 0);
}

#[test]
fn view_target_rejection_precedes_large_payload_copy_and_preserves_siblings() {
    let definition = view(Value::Str("x".repeat(4 << 20)));
    let budget = MemoryBudget::new(8 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let sibling = control.copy_text("independent").unwrap();
    let baseline = budget.used();
    let row_schema = schema(&control);
    let occupied = budget.reserve(budget.available() - (32 << 10)).unwrap();
    let observed = allocation_counter::measure(|| {
        let error = capture("public.v", &definition, row_schema, &control).unwrap_err();
        assert_eq!(error.sqlstate(), Some("53200"));
    });
    assert!(observed.bytes_total < 64 << 10, "{observed:?}");
    assert_eq!(budget.used(), baseline + occupied.bytes());
    assert_eq!(&**sibling, "independent");
    drop(occupied);
    let recovered = capture("public.v", &definition, schema(&control), &control).unwrap();
    drop(recovered);
    assert_eq!(budget.used(), baseline);
    drop(sibling);
    assert_eq!(budget.used(), 0);
}

#[test]
fn view_target_partial_capture_releases_names_queries_and_transferred_schemas() {
    let definition = view(Value::Str("x".repeat(2048)));
    let mut successes = 0;
    let mut failures = 0;
    for available in [0, 16, 64, 128, 256, 512, 1024, 4096, 8192] {
        let budget = MemoryBudget::new(1 << 20);
        let token = CancellationToken::new();
        let control = ProductionControl::new(&budget, &token, &token);
        let sibling = control.copy_text("independent").unwrap();
        let baseline = budget.used();
        let schema = schema(&control);
        let occupied = budget.reserve(budget.available() - available).unwrap();
        match capture("public.v", &definition, schema, &control) {
            Ok(output) => {
                successes += 1;
                drop(output);
            }
            Err(error) => {
                failures += 1;
                assert_eq!(error.sqlstate(), Some("53200"));
            }
        }
        assert_eq!(
            budget.used(),
            baseline + occupied.bytes(),
            "available={available}"
        );
        drop(occupied);
        drop(sibling);
        assert_eq!(budget.used(), 0);
    }
    assert!(successes > 0 && failures > 1);
}

#[test]
fn view_target_capture_observes_both_cancellations_and_releases_on_unwind() {
    let definition = view(Value::Str("x".repeat(1 << 20)));
    for original_cancelled in [true, false] {
        let budget = MemoryBudget::new(8 << 20);
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        let control = ProductionControl::new(&budget, &original, &invoking);
        let schema = schema(&control);
        if original_cancelled {
            original.cancel();
        } else {
            invoking.cancel();
        }
        let observed = allocation_counter::measure(|| {
            let error = capture("public.v", &definition, schema, &control).unwrap_err();
            assert_eq!(error.sqlstate(), Some("57014"));
        });
        assert!(observed.bytes_total < 1024, "{observed:?}");
        assert_eq!(budget.used(), 0);
    }
    let budget = MemoryBudget::new(8 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _target = capture("public.v", &definition, schema(&control), &control).unwrap();
        panic!("consumer unwind");
    }));
    assert!(result.is_err());
    assert_eq!(budget.used(), 0);
}
