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
        output = Some(rewrite(input, false, &control).unwrap());
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
        let error = rewrite(input, true, &control).unwrap_err();
        assert_eq!(
            error.sqlstate(),
            Some(if mode < 2 { "53200" } else { "57014" })
        );
        assert_eq!(budget.used(), sibling.bytes());
    }
}
