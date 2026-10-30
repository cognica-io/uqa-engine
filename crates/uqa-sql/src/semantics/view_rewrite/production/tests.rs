//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{plan::UnifiedPlan, semantics::view_rewrite::ViewQueryShape};
use uqa_core::{memory::MemoryBudget, CancellationToken, Value};

fn layer(expression: &ScalarExpr, control: &ProductionControl<'_>) -> OwnedViewQueryShape {
    let mut columns = ProductionVec::new(*control);
    for (name, physical) in [("first", "a"), ("second", "b")] {
        let column = OwnedViewColumn {
            name: control.copy_text(name).unwrap(),
            collation_oid: Some(100),
            expression: expression.copy_for_rewrite(control).unwrap(),
            writable_source_column: Some(control.copy_text(physical).unwrap()),
        };
        columns
            .push_produced(control.finish(column, control.empty_reservation()).unwrap())
            .unwrap();
    }
    let mut mapping = ProductionMap::new(*control);
    mapping.insert_copy("first", "a").unwrap();
    mapping.insert_copy("second", "b").unwrap();
    let UnifiedPlan::Query(query) =
        UnifiedPlan::lower(crate::compile("SELECT 7").unwrap().remove(0))
    else {
        panic!("query")
    };
    OwnedViewQueryShape::Updatable(OwnedAutomaticViewLayer {
        canonical_name: control.copy_text("public.v").unwrap(),
        source_name: control.copy_text("public.items").unwrap(),
        source_qualifier: control.copy_text("src").unwrap(),
        source_column_map: retain_column_mapping(&mapping, control).unwrap(),
        source_include_descendants: true,
        source_schema: RowSchema::new_with_control(
            control
                .finish(Vec::new(), control.empty_reservation())
                .unwrap(),
            control,
        )
        .unwrap(),
        columns: columns.finish().unwrap(),
        predicate: Some(
            ScalarExpr::Literal(Value::Bool(true))
                .copy_for_rewrite(control)
                .unwrap(),
        ),
        subqueries: ScalarQueryArena::copy_from(&[*query], control).unwrap(),
        check_option: ViewCheckOption::Cascaded,
    })
}

#[test]
fn readonly_view_shapes_move_payloads_and_retain_exact_live_allocations() {
    let expression = ScalarExpr::Literal(Value::Str("x".repeat(4 << 20)));
    let budget = MemoryBudget::new(32 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let input = layer(&expression, &control);
    let OwnedViewQueryShape::Updatable(source) = &input else {
        panic!("layer")
    };
    let payloads: Vec<_> = source
        .columns
        .iter()
        .map(|column| match &*column.expression {
            ScalarExpr::Literal(Value::Str(value)) => value.as_ptr(),
            _ => panic!("literal"),
        })
        .collect();
    let before = budget.used();
    let mut output = None;
    let allocation = allocation_counter::measure(|| output = Some(input.finish(&control).unwrap()));
    let output = output.unwrap();
    assert_eq!(
        before as i64 + allocation.bytes_current,
        budget.used() as i64
    );
    assert!(allocation.bytes_total < 64 << 10, "{allocation:?}");
    let ViewQueryShape::Updatable(layer) = &*output else {
        panic!("layer")
    };
    for (index, column) in layer.columns.iter().enumerate() {
        let ScalarExpr::Literal(Value::Str(value)) = &column.expression else {
            panic!("literal")
        };
        assert_eq!(value.as_ptr(), payloads[index]);
        assert_eq!(column.collation_oid, Some(100));
    }
    assert_eq!(layer.physical_source_column("first"), "a");
    assert_eq!(layer.physical_source_column("second"), "b");
    assert_eq!(layer.physical_source_column("unmapped"), "unmapped");
    assert_eq!(
        layer.columns[0].writable_source_column.as_deref(),
        Some("a")
    );
    assert_eq!(layer.check_option, ViewCheckOption::Cascaded);
    assert_eq!(layer.subqueries.len(), 1);
    drop(output);
    assert_eq!(budget.used(), 0);
}

#[test]
fn readonly_view_shape_rejection_retires_partial_destinations_and_preserves_siblings() {
    let mut failures = 0;
    let mut successes = 0;
    for available in [0, 32, 128, 256, 512, 1024, 4096, 16384] {
        let budget = MemoryBudget::new(1 << 20);
        let token = CancellationToken::new();
        let control = ProductionControl::new(&budget, &token, &token);
        let sibling = control.copy_text("independent").unwrap();
        let sibling_bytes = budget.used();
        let input = layer(&ScalarExpr::Literal(Value::Int(7)), &control);
        let occupied = budget.reserve(budget.available() - available).unwrap();
        let remaining = occupied.bytes() + sibling_bytes;
        match input.finish(&control) {
            Ok(output) => {
                successes += 1;
                drop(output);
            }
            Err(error) => {
                failures += 1;
                assert_eq!(error.sqlstate(), Some("53200"));
            }
        }
        assert_eq!(budget.used(), remaining, "available={available}");
        assert_eq!(&**sibling, "independent");
        drop(occupied);
        drop(sibling);
        assert_eq!(budget.used(), 0);
    }
    assert!(failures > 1 && successes > 0);
}

#[test]
fn readonly_view_shapes_preserve_both_cancellations_and_unwind_ownership() {
    for statement_cancelled in [true, false] {
        let budget = MemoryBudget::new(1 << 20);
        let statement = CancellationToken::new();
        let session = CancellationToken::new();
        let control = ProductionControl::new(&budget, &statement, &session);
        let sibling = control.copy_text("alive").unwrap();
        let before = budget.used();
        let input = layer(&ScalarExpr::Literal(Value::Int(9)), &control);
        if statement_cancelled {
            statement.cancel();
        } else {
            session.cancel();
        }
        let error = input.finish(&control).unwrap_err();
        assert_eq!(error.sqlstate(), Some("57014"));
        assert_eq!(budget.used(), before);
        drop(sibling);
        assert_eq!(budget.used(), 0);
    }
    let budget = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _output = layer(&ScalarExpr::Literal(Value::Int(4)), &control)
            .finish(&control)
            .unwrap();
        panic!("consumer unwinds");
    }));
    assert!(result.is_err());
    assert_eq!(budget.used(), 0);
}

#[test]
fn readonly_rejected_view_shapes_need_no_layer_storage() {
    let budget = MemoryBudget::new(0);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    for reason in [
        NotUpdatableReason::Distinct,
        NotUpdatableReason::GroupBy,
        NotUpdatableReason::Having,
        NotUpdatableReason::SetOperation,
        NotUpdatableReason::With,
        NotUpdatableReason::LimitOffset,
        NotUpdatableReason::Aggregate,
        NotUpdatableReason::Window,
        NotUpdatableReason::SetReturning,
        NotUpdatableReason::NotSingleRelation,
    ] {
        let result = OwnedViewQueryShape::NotUpdatable(reason)
            .finish(&control)
            .unwrap();
        assert!(matches!(*result, ViewQueryShape::NotUpdatable(actual) if actual == reason));
    }
    assert_eq!(budget.used(), 0);
}
