//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_sql::expr::RowLookup as _;

#[test]
fn floating_frame_offset_rejects_invalid_and_out_of_range_values() {
    assert_eq!(float_frame_offset(42.0).unwrap(), 42);
    for value in [
        f64::NAN,
        f64::INFINITY,
        -1.0,
        1.5,
        9_223_372_036_854_775_808.0,
    ] {
        assert!(float_frame_offset(value).is_err(), "{value}");
    }
}

#[test]
fn huge_partition_stays_disk_backed_with_a_tiny_output_budget() {
    let engine = Engine::new();
    let ctes = CteScope::new_for_current_routine(&engine);
    let hook = ScopedEngineHook::new(&engine, &ctes);
    let subqueries = Vec::new();
    let arena = PlanSubqueryArena::new(&subqueries, Some(&hook));
    let partition_schema = RowSchema::new(vec!["id".into(), "v".into()]);
    let mut partition = IndexedSpill::new(partition_schema.clone()).unwrap();
    for id in 0..4096_i64 {
        partition
            .push(&uqa_execution::PhysicalRow::from_values(vec![
                Value::Int(id),
                Value::Int(1),
            ]))
            .unwrap();
    }
    assert_eq!(partition.len(), 4096);
    assert!(partition.encoded_bytes() > 4096 * 8);

    let slot = WindowSlot {
        column: uqa_sql::ast::InternalRelationId::allocate().column(0),
        name: "sum".into(),
        args: vec![ScalarExpr::Column("v".into())],
        spec: ScalarWindowSpec {
            partition_by: Vec::new(),
            order_by: Vec::new(),
            frame: None,
        },
    };
    let schema = RowSchema::append_internal_typed(&partition_schema, &[(slot.column, None)]);
    let mut output = SpillBuffer::new(1);
    emit_window_partition(
        &slot,
        &mut partition,
        &schema,
        &mut output,
        &[],
        &hook,
        &arena,
    )
    .unwrap();

    assert!(output.has_spilled());
    assert!(output.in_memory_bytes() <= output.budget_bytes());
    assert_eq!(output.rows(), 4096);
    for batch in output.drain().unwrap() {
        let batch = batch.unwrap();
        for row in &batch.rows {
            assert_eq!(
                batch.schema.view(row).internal_column(slot.column),
                Some(&Value::Int(4096))
            );
        }
    }
}
