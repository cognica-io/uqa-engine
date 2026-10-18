//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::fdw_predicate;
use uqa_core::Value;
use uqa_sql::{
    ast::{FunctionBinding, FunctionCallSyntax, FunctionDispatch, PatternOperator},
    catalog::collations::CollationSelection,
    ScalarExpr,
};

fn pattern(operator: PatternOperator, collation: Option<u32>) -> ScalarExpr {
    let mut binding = FunctionBinding::dispatched(FunctionDispatch::Pattern {
        operator,
        oid: None,
    });
    binding.input_collation =
        collation.map(|oid| CollationSelection::from_oid(oid).expect("built-in collation"));
    ScalarExpr::Func {
        name: binding.name.clone(),
        binding: Some(binding),
        args: vec![
            ScalarExpr::Column("name".into()),
            ScalarExpr::Literal(Value::Str("a%".into())),
        ],
        order_syntax: FunctionCallSyntax::Ordinary,
        distinct: false,
        order_by: Vec::new(),
        filter: None,
    }
}

fn pushed(expression: &ScalarExpr) -> Option<uqa_fdw::PredicateOp> {
    fdw_predicate(expression, &[]).map(|predicate| {
        assert_eq!(predicate.column, "name");
        assert_eq!(predicate.value, Value::Str("a%".into()));
        predicate.operator
    })
}

#[test]
fn like_pushes_down_under_each_determinate_collation() {
    for collation in [
        None,
        Some(100),
        Some(811),
        Some(950),
        Some(951),
        Some(962),
        Some(963),
        Some(6411),
    ] {
        assert_eq!(
            pushed(&pattern(PatternOperator::Like, collation)),
            Some(uqa_fdw::PredicateOp::Like),
            "{collation:?}"
        );
        assert_eq!(
            pushed(&pattern(PatternOperator::NotLike, collation)),
            Some(uqa_fdw::PredicateOp::NotLike),
            "{collation:?}"
        );
        assert_eq!(
            pushed(&ScalarExpr::Not(Box::new(pattern(
                PatternOperator::Like,
                collation
            )))),
            Some(uqa_fdw::PredicateOp::NotLike),
            "{collation:?}"
        );
        assert_eq!(
            pushed(&ScalarExpr::Not(Box::new(pattern(
                PatternOperator::NotLike,
                collation
            )))),
            Some(uqa_fdw::PredicateOp::Like),
            "{collation:?}"
        );
    }
}

#[test]
fn collation_dependent_patterns_stay_in_the_canonical_operator() {
    // An indeterminate collation is reported by the operator for each row it evaluates.
    for operator in [PatternOperator::Like, PatternOperator::NotLike] {
        assert_eq!(pushed(&pattern(operator, Some(0))), None);
    }
    // ILIKE lowercases through its collation's provider, which no handler reproduces.
    for collation in [None, Some(0), Some(811), Some(950), Some(963), Some(6411)] {
        for operator in [PatternOperator::ILike, PatternOperator::NotILike] {
            assert_eq!(pushed(&pattern(operator, collation)), None, "{collation:?}");
            assert_eq!(
                pushed(&ScalarExpr::Not(Box::new(pattern(operator, collation)))),
                None,
                "{collation:?}"
            );
        }
    }
}
