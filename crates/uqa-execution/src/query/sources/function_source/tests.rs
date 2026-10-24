//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::query::set_projection::SetFunctionRuntime;
use crate::{OwnedPhysicalRow, PhysicalRow, RowSchema, ScalarExpr};
use std::sync::{atomic::Ordering, Arc};
use uqa_core::Value;

use super::test_support as runtime;

#[test]
fn deferred_function_calls_share_immutable_inputs_and_read_current_outer_rows() {
    let arguments = vec![ScalarExpr::Literal(Value::Str("x".repeat(64 * 1024)))];
    let call = TableFunctionCall {
        name: "correlated_payload",
        binding: None,
        output_name: "correlated_payload",
        relations: None,
        args: &arguments,
        function_states: None,
        routine_descriptions: None,
        alias: None,
        column_aliases: &[],
        ordinality: false,
        column_types: &[],
    };
    let schema = RowSchema::new(vec!["input".into()]);
    let observer = Arc::new(runtime::Runtime::default());
    let runtime: Arc<dyn SetFunctionRuntime> = observer.clone();
    let allocation = allocation_counter::measure(|| {
        let call = Arc::new(PreparedFunctionCall::new(&call, None));
        for expected in 0..512 {
            let outer = OwnedPhysicalRow::new(
                schema.clone(),
                PhysicalRow::from_values(vec![Value::Int(expected)]),
            );
            let mut rows = deferred_function_rows(
                Arc::clone(&call),
                Arc::clone(&runtime),
                &[],
                Some(outer),
                2,
            );
            assert_eq!(observer.calls.load(Ordering::Relaxed), expected as usize);
            let row = rows.next().unwrap().unwrap();
            assert_eq!(row.value(0), Some(&Value::Int(expected)));
            assert_eq!(row.value(1), Some(&Value::Int(64 * 1024)));
            assert!(rows.next().is_none());
            assert_eq!(
                observer.calls.load(Ordering::Relaxed),
                expected as usize + 1
            );
        }
        drop(deferred_function_rows(
            Arc::clone(&call),
            runtime,
            &[],
            None,
            2,
        ));
        assert_eq!(observer.calls.load(Ordering::Relaxed), 512);
    });
    // Preparing one immutable 64 KiB argument must not copy it for each of 512 outer rows.
    assert!(allocation.bytes_total <= 1024 * 1024, "{allocation:?}");
    eprintln!("shared function inputs: {allocation:?}");
}
