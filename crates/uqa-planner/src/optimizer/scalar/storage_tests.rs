//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_sql::ast::BinaryOp;

fn column() -> ScalarExpr {
    ScalarExpr::Column("value".into())
}

fn config() -> OptimizerConfig<'static> {
    OptimizerConfig::new(uqa_execution::scalar::eval_constant_scalar_with_control)
}

#[test]
fn scalar_rewrite_reuses_surviving_boxes_and_array_capacity() {
    let input = ScalarExpr::Array(vec![
        ScalarExpr::Binary {
            op: BinaryOp::Add,
            lhs: Box::new(column()),
            rhs: Box::new(column()),
        },
        ScalarExpr::UnaryMinus(Box::new(column())),
        ScalarExpr::Not(Box::new(column())),
        ScalarExpr::IsNull {
            expr: Box::new(column()),
            negated: false,
        },
        ScalarExpr::Between {
            expr: Box::new(column()),
            low: Box::new(column()),
            high: Box::new(column()),
        },
        ScalarExpr::InList {
            expr: Box::new(column()),
            list: vec![column()],
            negated: false,
        },
        ScalarExpr::InSubquery {
            expr: Box::new(column()),
            subquery: 0,
            negated: false,
        },
    ]);
    let expected = input.clone();
    let mut output = None;
    let allocation = allocation_counter::measure(|| {
        output = Some(optimize_scalar(input, &config()).unwrap());
    });
    assert_eq!(allocation.bytes_total, 0, "{allocation:?}");
    assert_eq!(output.unwrap(), expected);
}

#[test]
fn case_reuses_storage_without_evaluating_unreachable_values() {
    for coerced in [false, true] {
        let mut config = config();
        config.coerced_conditionals = coerced;
        let when = vec![
            (ScalarExpr::Literal(Value::Bool(false)), invalid_cast()),
            (column(), column()),
            (ScalarExpr::Literal(Value::Bool(true)), column()),
            (invalid_cast(), invalid_cast()),
        ];
        let buffer = when.as_ptr();
        let branch = Box::new(invalid_cast());
        let branch_buffer = std::ptr::from_ref(branch.as_ref());
        let input = ScalarExpr::Case {
            base: None,
            when,
            else_branch: Some(branch),
        };
        let output = optimize_scalar(input, &config).unwrap();
        let ScalarExpr::Case {
            when, else_branch, ..
        } = output
        else {
            panic!("case")
        };
        assert_eq!(when.as_ptr(), buffer);
        assert_eq!(
            std::ptr::from_ref(else_branch.as_deref().unwrap()),
            branch_buffer
        );
        if coerced {
            assert_eq!(when, vec![(column(), column())]);
            assert_eq!(*else_branch.unwrap(), column());
        } else {
            assert_eq!(when.len(), 4);
            assert_eq!(when[0].1, invalid_cast());
            assert_eq!(when[3], (invalid_cast(), invalid_cast()));
            assert_eq!(*else_branch.unwrap(), invalid_cast());
        }
    }
}

#[test]
fn coalesce_reuses_arguments_and_keeps_unbound_type_arms() {
    for coerced in [false, true] {
        let mut config = config();
        config.coerced_conditionals = coerced;
        let args = vec![
            column(),
            ScalarExpr::Literal(Value::Null),
            ScalarExpr::Literal(Value::Bool(true)),
            invalid_cast(),
        ];
        let buffer = args.as_ptr();
        let output = optimize_coalesce(args, &config).unwrap();
        assert_eq!(output.as_ptr(), buffer);
        assert_eq!(output.len(), if coerced { 3 } else { 4 });
        assert_eq!(output[2], ScalarExpr::Literal(Value::Bool(true)));
        if !coerced {
            assert_eq!(output[3], invalid_cast());
        }
    }
}

#[test]
fn simple_case_moves_base_and_else_boxes_without_changing_arm_order() {
    let base = Box::new(column());
    let branch = Box::new(column());
    let base_buffer = std::ptr::from_ref(base.as_ref());
    let branch_buffer = std::ptr::from_ref(branch.as_ref());
    let when = vec![(column(), column()), (column(), column())];
    let when_buffer = when.as_ptr();
    let output = optimize_case(Some(base), when, Some(branch), &config()).unwrap();
    let ScalarExpr::Case {
        base,
        when,
        else_branch,
    } = output
    else {
        panic!("case")
    };
    assert_eq!(std::ptr::from_ref(base.as_deref().unwrap()), base_buffer);
    assert_eq!(
        std::ptr::from_ref(else_branch.as_deref().unwrap()),
        branch_buffer
    );
    assert_eq!(when.as_ptr(), when_buffer);
}

fn invalid_cast() -> ScalarExpr {
    ScalarExpr::Cast {
        expr: Box::new(ScalarExpr::Literal(Value::Str("invalid".into()))),
        ty: "integer".into(),
        implicit: false,
    }
}
