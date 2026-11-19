//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::{memory::MemoryBudget, CancellationToken};

fn expression(sql: &str) -> Expr {
    let crate::Statement::CreateTable(mut table) =
        crate::compile(&format!("CREATE TABLE t(a bigint DEFAULT ({sql}))"))
            .unwrap()
            .remove(0)
    else {
        panic!("table")
    };
    table.columns.remove(0).default.unwrap()
}

#[test]
fn scalar_sequence_observation_preserves_selected_builtins_and_runtime_inputs() {
    for (sql, expected) in [
        ("nextval('public.original')", true),
        ("currval('public.original'::regclass)", true),
        ("setval('public.original'::text,1)", false),
        ("nextval(2147483647::regclass)", false),
    ] {
        let memory = MemoryBudget::new(64 << 10);
        let token = CancellationToken::new();
        let control = ProductionControl::new(&memory, &token, &token);
        let scalar =
            crate::plan::ExpressionPlan::lower_scalar_with_control(&expression(sql), &control)
                .unwrap();
        let baseline = memory.used();
        let mut count = 0;
        visit_scalar_sequence_function_references_with_control(&scalar, &control, &mut |name| {
            assert_eq!(name, "public.original");
            count += 1;
            Ok(())
        })
        .unwrap();
        assert_eq!(count, usize::from(expected), "{sql}");
        assert_eq!(memory.used(), baseline);
    }
    let mut call = expression("nextval('public.original')");
    let Expr::Func { binding, name, .. } = &mut call else {
        panic!("function")
    };
    *name = "historical_display_name".into();
    *binding = Some(crate::ast::FunctionBinding {
        name: "pg_catalog.nextval".into(),
        argument_types: vec!["regclass".into()],
        builtin: true,
        object_id: None,
        dispatch: None,
        invocation: None,
        value_binding: None,
        resolution_error: None,
        input_collation: None,
    });
    for user_function in [false, true] {
        let Expr::Func {
            binding: Some(binding),
            ..
        } = &mut call
        else {
            unreachable!()
        };
        binding.builtin = !user_function;
        binding.object_id = user_function.then_some([1; 16]);
        let control = ProductionControl::uncontrolled();
        let scalar =
            crate::plan::ExpressionPlan::lower_scalar_with_control(&call, &control).unwrap();
        let mut count = 0;
        visit_scalar_sequence_function_references_with_control(&scalar, &control, &mut |_| {
            count += 1;
            Ok(())
        })
        .unwrap();
        assert_eq!(count, usize::from(!user_function));
    }
}

#[test]
fn scalar_sequence_observation_borrows_payloads_and_preserves_control_errors() {
    let source = ScalarExpr::Array(vec![
        ScalarExpr::Literal(Value::Str("x".repeat(4 << 20))),
        crate::plan::ExpressionPlan::lower_scalar_with_control(
            &expression("nextval('public.original')"),
            &ProductionControl::uncontrolled(),
        )
        .unwrap()
        .into_uncontrolled()
        .unwrap(),
    ]);
    let memory = MemoryBudget::new(0);
    let statement = CancellationToken::new();
    let session = CancellationToken::new();
    let control = ProductionControl::new(&memory, &statement, &session);
    let measured = allocation_counter::measure(|| {
        visit_scalar_sequence_function_references_with_control(&source, &control, &mut |name| {
            assert_eq!(name, "public.original");
            Ok(())
        })
        .unwrap();
    });
    assert_eq!(measured.count_total, 0, "{measured:?}");
    let error =
        visit_scalar_sequence_function_references_with_control(&source, &control, &mut |_| {
            Err(SQLError::Internal("stopped".into()))
        })
        .unwrap_err();
    assert!(error.to_string().contains("stopped"));
    for token in [&statement, &session] {
        token.cancel();
        assert_eq!(
            visit_scalar_sequence_function_references_with_control(
                &source,
                &control,
                &mut |_| panic!("cancelled")
            )
            .unwrap_err()
            .sqlstate(),
            Some("57014")
        );
        token.reset();
    }
    assert_eq!(memory.used(), 0);
}

#[test]
fn sequence_observation_borrows_large_unrelated_payloads_without_allocating() {
    let expression = Expr::Array(vec![
        Expr::Literal(Value::Str("x".repeat(4 << 20))),
        expression("nextval('first')"),
        expression("currval('second')"),
    ]);
    let budget = MemoryBudget::new(0);
    let statement = CancellationToken::new();
    let session = CancellationToken::new();
    let control = ProductionControl::new(&budget, &statement, &session);
    let mut names = Vec::with_capacity(2);
    let measured = allocation_counter::measure(|| {
        visit_sequence_function_references_with_control(&expression, &control, &mut |reference| {
            names.push(if reference == "first" {
                1
            } else if reference == "second" {
                2
            } else {
                panic!("reference")
            });
            Ok(())
        })
        .unwrap();
    });
    assert_eq!(names, [1, 2]);
    assert_eq!(measured.count_total, 0, "{measured:?}");
    assert_eq!(budget.used(), 0);
    for token in [&statement, &session] {
        token.cancel();
        assert_eq!(
            visit_sequence_function_references_with_control(
                &expression,
                &control,
                &mut |_| panic!("cancelled traversal")
            )
            .unwrap_err()
            .sqlstate(),
            Some("57014")
        );
        token.reset();
    }
}

#[test]
fn sequence_observation_preserves_child_order_and_stops_at_the_first_callback_error() {
    let source = expression("sum(nextval('argument')) FILTER(WHERE currval('filter')>0) OVER(PARTITION BY nextval('partition') ORDER BY currval('ordering') ROWS BETWEEN nextval('start') PRECEDING AND nextval('end') FOLLOWING)");
    let expected = [
        "argument",
        "filter",
        "partition",
        "ordering",
        "start",
        "end",
    ];
    let mut ordinary = source.clone();
    let mut observed = Vec::new();
    super::super::rewrite_sequence_function_references(&mut ordinary, &mut |reference| {
        observed.push(reference.clone());
        Ok(())
    })
    .unwrap();
    assert_eq!(observed, expected);
    let budget = MemoryBudget::new(4096);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let mut count = 0;
    visit_sequence_function_references_with_control(&source, &control, &mut |reference| {
        assert_eq!(reference, expected[count]);
        count += 1;
        Ok(())
    })
    .unwrap();
    assert_eq!(count, expected.len());
    for stop in 0..expected.len() {
        let mut count = 0;
        let error =
            visit_sequence_function_references_with_control(&source, &control, &mut |reference| {
                assert_eq!(reference, expected[count]);
                count += 1;
                if count == stop + 1 {
                    Err(SQLError::Internal("callback failed".into()))
                } else {
                    Ok(())
                }
            })
            .unwrap_err();
        assert!(error.to_string().contains("callback failed"));
        assert_eq!(count, stop + 1);
        assert_eq!(budget.used(), 0);
    }
    let subquery = expression("nextval('before')+(SELECT 1)+nextval('after')");
    let mut visited = 0;
    let error =
        visit_sequence_function_references_with_control(&subquery, &control, &mut |reference| {
            assert_eq!(reference, "before");
            visited += 1;
            Ok(())
        })
        .unwrap_err();
    assert_eq!(visited, 1);
    assert!(error
        .to_string()
        .contains("schema expression contains a subquery"));
    assert_eq!(budget.used(), 0);
}

#[test]
fn stored_sequence_reference_errors_admit_their_selected_text_before_copying() {
    use crate::schema::sequences::names::{
        resolve_stored_sequence_reference, resolve_stored_sequence_reference_with_control,
        StoredSequenceRegistry,
    };
    struct Empty;
    impl StoredSequenceRegistry for Empty {
        fn contains_sequence(&self, _: &uqa_core::RelationIdentity) -> bool {
            false
        }
        fn sequence_names(&self) -> Vec<uqa_core::RelationIdentity> {
            Vec::new()
        }
    }
    let budget = MemoryBudget::new(64 << 10);
    let statement = CancellationToken::new();
    let session = CancellationToken::new();
    let control = ProductionControl::new(&budget, &statement, &session);
    let sibling = control.copy_text("sibling").unwrap();
    let baseline = budget.used();
    for reference in ["x".repeat(4 << 20), format!("\"{}", "x".repeat(4 << 20))] {
        let measured = allocation_counter::measure(|| {
            assert_eq!(
                resolve_stored_sequence_reference_with_control(&Empty, &reference, &control)
                    .unwrap_err()
                    .sqlstate(),
                Some("53200")
            );
        });
        // Diagnostic formatting streams admitted chunks until the allowance is exhausted.
        assert!(measured.bytes_max <= 64 << 10, "{measured:?}");
        assert!(measured.bytes_total < 128 << 10, "{measured:?}");
        assert_eq!(budget.used(), baseline);
    }
    for reference in ["missing", "\"invalid"] {
        let ordinary = resolve_stored_sequence_reference(&Empty, reference).unwrap_err();
        let controlled =
            resolve_stored_sequence_reference_with_control(&Empty, reference, &control)
                .unwrap_err();
        assert_eq!(
            controlled.to_string(),
            SQLError::Internal(ordinary).to_string()
        );
        assert_eq!(budget.used(), baseline);
    }
    for token in [&statement, &session] {
        token.cancel();
        assert_eq!(
            resolve_stored_sequence_reference_with_control(&Empty, "missing", &control)
                .unwrap_err()
                .sqlstate(),
            Some("57014")
        );
        assert_eq!(budget.used(), baseline);
        token.reset();
    }
    assert_eq!(&**sibling, "sibling");
    drop(sibling);
    assert_eq!(budget.used(), 0);
}
