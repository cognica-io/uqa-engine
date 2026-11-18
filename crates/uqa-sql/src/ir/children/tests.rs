//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{
    ast::{FrameExclusion, FrameMode, WindowCallModifiers},
    ScalarWindowFrame, ScalarWindowSpec,
};

fn parameter(value: &ScalarExpr) -> usize {
    let ScalarExpr::Param(index) = value else {
        panic!("parameter child")
    };
    *index
}

fn check(mut expression: ScalarExpr, scope: Scope, expected: &[usize]) {
    let allocation = allocation_counter::measure(|| {
        let mut children = expression.children(scope);
        for (position, value) in expected.iter().enumerate() {
            assert_eq!(children.len(), expected.len() - position);
            assert_eq!(parameter(children.next().unwrap()), *value);
        }
        assert_eq!(children.len(), 0);
        assert!(children.next().is_none());
        for (index, expected) in expected.iter().enumerate() {
            assert_eq!(
                parameter(expression.children(scope).nth(index).unwrap()),
                *expected
            );
            assert_eq!(
                parameter(expression.children_mut(scope).nth(index).unwrap()),
                *expected
            );
        }
        assert!(expression.children(scope).nth(usize::MAX).is_none());
        assert!(expression.children_mut(scope).nth(usize::MAX).is_none());
        for child in expression.children_mut(scope) {
            let ScalarExpr::Param(index) = child else {
                panic!("parameter child")
            };
            *index += 10;
        }
        for (child, expected) in expression.children(scope).zip(expected) {
            assert_eq!(parameter(child), expected + 10);
        }
    });
    assert_eq!(allocation.count_total, 0, "{allocation:?}");
}

#[test]
fn scalar_child_borrows_preserve_window_and_call_ownership_order() {
    let expression = ScalarExpr::WindowCall {
        name: "sum".into(),
        args: vec![ScalarExpr::Param(0), ScalarExpr::Param(1)],
        filter: Some(Box::new(ScalarExpr::Param(2))),
        spec: ScalarWindowSpec {
            definition: None,
            partition_by: vec![ScalarExpr::Param(3)],
            order_by: vec![ScalarOrder {
                expr: ScalarExpr::Param(4),
                descending: false,
                nulls: None,
            }],
            frame: Some(ScalarWindowFrame {
                mode: FrameMode::Rows,
                start: ScalarFrameBound::Preceding(Box::new(ScalarExpr::Param(5))),
                end: ScalarFrameBound::Following(Box::new(ScalarExpr::Param(6))),
                between: true,
                exclusion: FrameExclusion::NoOthers,
            }),
        },
        modifiers: WindowCallModifiers::default(),
    };
    check(expression.clone(), Scope::Complete, &[0, 1, 2, 3, 4, 5, 6]);
    check(
        expression.clone(),
        Scope::Construction,
        &[0, 1, 3, 4, 5, 6, 2],
    );
    check(expression, Scope::Observation { call_order: true }, &[]);
    let expression = ScalarExpr::Func {
        name: "sum".into(),
        binding: None,
        args: vec![ScalarExpr::Param(0)],
        distinct: false,
        order_syntax: crate::ast::FunctionOrderSyntax::default(),
        order_by: vec![ScalarOrder {
            expr: ScalarExpr::Param(1),
            descending: false,
            nulls: None,
        }],
        filter: Some(Box::new(ScalarExpr::Param(2))),
    };
    check(expression.clone(), Scope::Complete, &[0, 1, 2]);
    check(
        expression.clone(),
        Scope::Observation { call_order: true },
        &[0, 1, 2],
    );
    check(
        expression,
        Scope::Observation { call_order: false },
        &[0, 2],
    );
}

#[test]
fn scalar_case_children_skip_pairs_without_losing_pending_results() {
    let expression = ScalarExpr::Case {
        base: Some(Box::new(ScalarExpr::Param(0))),
        when: vec![
            (ScalarExpr::Param(1), ScalarExpr::Param(2)),
            (ScalarExpr::Param(3), ScalarExpr::Param(4)),
        ],
        else_branch: Some(Box::new(ScalarExpr::Param(5))),
    };
    check(expression.clone(), Scope::Complete, &[0, 1, 2, 3, 4, 5]);
    for consumed in 0..=6 {
        for skipped in 0..=7 {
            let mut expression = expression.clone();
            let mut children = expression.children_mut(Scope::Complete);
            for index in 0..consumed {
                assert_eq!(parameter(children.next().unwrap()), index);
            }
            let selected = consumed + skipped;
            assert_eq!(
                children.nth(skipped).map(|value| parameter(value)),
                (selected < 6).then_some(selected)
            );
            assert_eq!(children.len(), 6_usize.saturating_sub(selected + 1));
            assert_eq!(
                children.map(|value| parameter(value)).collect::<Vec<_>>(),
                ((selected + 1).min(6)..6).collect::<Vec<_>>()
            );
        }
    }
}
