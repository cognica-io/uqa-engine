//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::{memory::MemoryBudget, CancellationToken, Value};
use uqa_sql::ast::FunctionOrderSyntax;

fn call(name: &str, qualifier: Option<&str>) -> ScalarExpr {
    ScalarExpr::Func {
        name: name.into(),
        binding: None,
        args: vec![
            qualifier.map_or_else(
                || ScalarExpr::Column("field".into()),
                |qualifier| ScalarExpr::QualifiedColumn {
                    qualifier: qualifier.into(),
                    column: "field".into(),
                },
            ),
            ScalarExpr::Literal(Value::Str("payload".repeat(1024))),
        ],
        distinct: false,
        order_by: Vec::new(),
        order_syntax: FunctionOrderSyntax::Ordinary,
        filter: None,
    }
}

#[test]
fn fusion_transfers_payloads_and_retains_only_exact_surviving_constructors() {
    let source = ScalarExpr::And(vec![
        call("TEXT_MATCH", Some("items")),
        ScalarExpr::And(vec![
            call("knn_match", Some("items")),
            ScalarExpr::Column("residual".into()),
        ]),
    ]);
    let budget = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let mut output = None;
    let allocations = allocation_counter::measure(|| {
        let input = source.copy_for_rewrite(&control).unwrap();
        output = Some(rewrite(input, false, &control, &control).unwrap());
    });
    let output = output.unwrap();
    let ScalarExpr::And(children) = &*output else {
        panic!("residual conjunction")
    };
    let ScalarExpr::Func { name, args, .. } = &children[0] else {
        panic!("fusion")
    };
    assert_eq!(name, "fuse_bayesian_evidence");
    assert_eq!(args.len(), 2);
    assert!(matches!(&args[0], ScalarExpr::Func { name, .. } if name == "bayesian_match"));
    assert_eq!(children[1], ScalarExpr::Column("residual".into()));
    assert_eq!(
        allocations.bytes_current,
        i64::try_from(budget.used()).unwrap()
    );
    assert_eq!(output.reserved_bytes(), budget.used());
    drop(output);
    assert_eq!(budget.used(), 0);
}

#[test]
fn calibration_renames_the_call_in_its_existing_argument_storage() {
    let budget = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let mut source = call("text_match", Some("items"));
    let ScalarExpr::Func { binding, .. } = &mut source else {
        panic!("call")
    };
    *binding = Some(uqa_sql::ast::FunctionBinding::builtin_by_name(
        "text_match",
        None,
    ));
    let input = source.copy_for_rewrite(&control).unwrap();
    let ScalarExpr::Func {
        args,
        name,
        binding: Some(binding),
        ..
    } = &*input
    else {
        panic!("call")
    };
    let arguments = args.as_ptr();
    let selected_name = binding.name.as_ptr();
    let original_capacity = name.capacity();
    let before = input.reserved_bytes();
    let sibling = budget.reserve(budget.available() - 32).unwrap();
    let output = calibrate(input, &control, &control).unwrap();
    let ScalarExpr::Func {
        args,
        name,
        binding: Some(binding),
        ..
    } = &*output
    else {
        panic!("call")
    };
    assert_eq!(name, "bayesian_match");
    assert_eq!(args.as_ptr(), arguments);
    assert_eq!(binding.name, "text_match");
    assert_eq!(binding.name.as_ptr(), selected_name);
    assert_eq!(
        output.reserved_bytes(),
        before - original_capacity + name.capacity()
    );
    drop((output, sibling));
    assert_eq!(budget.used(), 0);
}

#[test]
fn caller_owned_fusion_charges_construction_and_preserves_existing_arguments() {
    for (limit, cancel_original, cancel_invoking) in [
        (0, false, false),
        (32 * 1024, true, false),
        (32 * 1024, false, true),
        (32 * 1024, false, false),
    ] {
        let budget = MemoryBudget::new(limit);
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        let mut config = crate::optimizer::OptimizerConfig::new(
            uqa_execution::scalar::eval_constant_scalar_with_control,
        );
        config.production = ProductionControl::new(&budget, &original, &invoking);
        let mut text = call("text_match", Some("items"));
        let ScalarExpr::Func { args, .. } = &mut text else {
            panic!("text call")
        };
        let payload = "x".repeat(1 << 20);
        let payload_pointer = payload.as_ptr();
        args[1] = ScalarExpr::Literal(Value::Str(payload));
        let argument_pointer = args.as_ptr();
        let mut predicate = ScalarExpr::And(vec![text, call("knn_match", Some("items"))]);
        if cancel_original {
            original.cancel();
        }
        if cancel_invoking {
            invoking.cancel();
        }
        let result = crate::optimizer::scalar::rewrite_ordinary_slot(
            &mut predicate,
            &config,
            |input, production| {
                super::rewrite(input, true, &production.storage, &config.production)
            },
        );
        if limit == 0 || cancel_original || cancel_invoking {
            assert_eq!(
                result.unwrap_err().sqlstate(),
                Some(if limit == 0 { "53200" } else { "57014" })
            );
        } else {
            result.unwrap();
            let ScalarExpr::Func { name, args, .. } = &predicate else {
                panic!("fusion")
            };
            assert_eq!(name, "fuse_bayesian_evidence");
            let ScalarExpr::Func { name, args, .. } = &args[0] else {
                panic!("calibrated call")
            };
            assert_eq!(name, "bayesian_match");
            assert_eq!(args.as_ptr(), argument_pointer);
            let ScalarExpr::Literal(Value::Str(text)) = &args[1] else {
                panic!("text argument")
            };
            assert_eq!(text.as_ptr(), payload_pointer);
            assert!(budget.peak() > 0);
        }
        assert_eq!(budget.used(), 0);
    }
}

#[test]
fn rejected_and_cancelled_fusion_releases_private_children_and_scratch() {
    let source = ScalarExpr::And(vec![call("text_match", None), call("knn_match", None)]);
    for mode in 0..4 {
        let budget = MemoryBudget::new(1 << 20);
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        let control = ProductionControl::new(&budget, &original, &invoking);
        let input = source.copy_for_rewrite(&control).unwrap();
        let spare = if mode == 1 {
            size_of::<OwnedScalarExpr>()
        } else {
            0
        };
        let sibling = budget.reserve(budget.available() - spare).unwrap();
        if mode == 2 {
            original.cancel();
        }
        if mode == 3 {
            invoking.cancel();
        }
        let error = rewrite(input, true, &control, &control).unwrap_err();
        assert_eq!(
            error.sqlstate(),
            Some(if mode < 2 { "53200" } else { "57014" })
        );
        assert_eq!(budget.used(), sibling.bytes());
    }
}
