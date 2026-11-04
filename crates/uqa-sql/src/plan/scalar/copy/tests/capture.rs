//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::ir::{RetainedConstantValue, RetainedScalarLiteral};

#[test]
fn scalar_capture_detaches_nested_evaluated_constants_without_changing_ordinary_copies() {
    for uncontrolled in [false, true] {
        let original_memory = MemoryBudget::new(1 << 20);
        let original_token = CancellationToken::new();
        let original_control =
            ProductionControl::new(&original_memory, &original_token, &original_token);
        let expected = Value::Str("evaluated".repeat(4096));
        let value = RetainedConstantValue::from_produced(
            original_control.copy_value(&expected).unwrap(),
            &original_control,
        )
        .unwrap();
        let literal = RetainedScalarLiteral::with_control(
            value,
            Some(
                ColumnType::Text
                    .clone_with_control(&original_control)
                    .unwrap(),
            ),
            Some(original_control.copy_text("text").unwrap()),
            Some(950),
            &original_control,
        )
        .unwrap();
        let mut source = ScalarExpr::RetainedLiteral(literal);
        for _ in 0..128 {
            source = ScalarExpr::IsNull {
                expr: Box::new(source),
                negated: true,
            };
        }
        original_token.cancel();
        let memory = MemoryBudget::new(1 << 20);
        let token = CancellationToken::new();
        let control = if uncontrolled {
            ProductionControl::uncontrolled()
        } else {
            ProductionControl::new(&memory, &token, &token)
        };
        let ordinary = source.clone_with_control(&control).unwrap();
        let mut saved = None;
        let allocations = allocation_counter::measure(|| {
            saved = Some(source.capture_with_control(&control).unwrap());
        });
        let saved = saved.unwrap();
        if !uncontrolled {
            // Core admits both shared payloads; their two pairs of Arc reference counters remain outside the payload allowance.
            assert_eq!(
                allocations.bytes_current,
                i64::try_from(memory.used() - ordinary.reserved_bytes() + 4 * size_of::<usize>())
                    .unwrap()
            );
        }
        drop(source);
        assert!(original_memory.used() > 0);
        drop(ordinary);
        assert_eq!(original_memory.used(), 0);
        let mut leaf = &*saved;
        for _ in 0..128 {
            let ScalarExpr::IsNull { expr, .. } = leaf else {
                panic!("condition")
            };
            leaf = expr;
        }
        let ScalarExpr::RetainedLiteral(literal) = leaf else {
            panic!("constant")
        };
        assert!(literal
            .value
            .resident_value()
            .unwrap()
            .has_same_representation(&expected));
        assert_eq!(literal.ty, Some(ColumnType::Text));
        assert_eq!(literal.declaration.as_deref(), Some("text"));
        assert_eq!(literal.collation_oid, Some(950));
        drop(saved);
        assert_eq!(memory.used(), 0);
    }
}

#[test]
fn scalar_capture_releases_prior_constants_on_payload_denial_and_preserves_read_errors() {
    #[derive(Debug)]
    struct Broken;
    impl crate::ir::ConstantValueSource for Broken {
        fn read(&self, _: u64) -> Result<Value, crate::SQLError> {
            Err(crate::SQLError::Routine {
                sqlstate: "XX001".into(),
                message: "unreadable retained fixture".into(),
            })
        }
    }
    let literal = || {
        ScalarExpr::RetainedLiteral(RetainedScalarLiteral::new(
            RetainedConstantValue::from_produced(
                ProductionControl::uncontrolled()
                    .copy_value(&Value::Str("first".repeat(40)))
                    .unwrap(),
                &ProductionControl::uncontrolled(),
            )
            .unwrap(),
            None,
            None,
            None,
        ))
    };
    let broken = ScalarExpr::RetainedLiteral(RetainedScalarLiteral::new(
        RetainedConstantValue::new(std::sync::Arc::new(Broken), 0),
        None,
        None,
        None,
    ));
    for (last, state) in [
        (ScalarExpr::Literal(Value::Str("x".repeat(65536))), "53200"),
        (broken, "XX001"),
    ] {
        let source = ScalarExpr::Row(vec![literal(), last]);
        let memory = MemoryBudget::new(4096);
        let token = CancellationToken::new();
        let error: crate::SQLError = source
            .capture_with_control(&ProductionControl::new(&memory, &token, &token))
            .unwrap_err()
            .into();
        assert_eq!(error.sqlstate(), Some(state));
        assert_eq!(memory.used(), 0);
        for original in [false, true] {
            let first = CancellationToken::new();
            let second = CancellationToken::new();
            if original {
                first.cancel();
            } else {
                second.cancel();
            }
            assert!(source
                .capture_with_control(&ProductionControl::new(&memory, &first, &second))
                .is_err());
            assert_eq!(memory.used(), 0);
        }
    }
}
