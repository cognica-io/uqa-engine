//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{text, ScalarExpr};
use crate::{
    ast::{
        BinaryOp, ColumnType, CompositeRowBinding, FrameExclusion, FrameMode, FunctionOrderSyntax,
        WindowCallModifiers,
    },
    ir::{ResultCollation, ScalarFrameBound, ScalarOrder, ScalarWindowFrame, ScalarWindowSpec},
};

fn items() -> Vec<ScalarExpr> {
    vec![text(101), text(203), text(307)]
}

pub(super) fn child_shapes() -> Vec<(ScalarExpr, Vec<ScalarExpr>)> {
    let mut shapes = vec![
        (ScalarExpr::Array(items()), items()),
        (ScalarExpr::Row(items()), items()),
        (ScalarExpr::And(items()), items()),
        (ScalarExpr::Or(items()), items()),
        (ScalarExpr::UnaryMinus(Box::new(text(101))), vec![text(101)]),
        (ScalarExpr::Not(Box::new(text(101))), vec![text(101)]),
        (
            ScalarExpr::IsNull {
                expr: Box::new(text(101)),
                negated: true,
            },
            vec![text(101)],
        ),
        (
            ScalarExpr::Cast {
                expr: Box::new(text(101)),
                ty: "long composite type".repeat(20),
                implicit: false,
            },
            vec![text(101)],
        ),
        (
            ScalarExpr::InSubquery {
                expr: Box::new(text(101)),
                subquery: 7,
                negated: false,
            },
            vec![text(101)],
        ),
        (
            ScalarExpr::Binary {
                op: BinaryOp::Add,
                lhs: Box::new(text(101)),
                rhs: Box::new(text(203)),
            },
            vec![text(101), text(203)],
        ),
        (
            ScalarExpr::Between {
                expr: Box::new(text(101)),
                low: Box::new(text(203)),
                high: Box::new(text(307)),
            },
            items(),
        ),
        (
            ScalarExpr::InList {
                expr: Box::new(text(101)),
                list: vec![text(203), text(307)],
                negated: true,
            },
            items(),
        ),
        (
            ScalarExpr::CompositeRow {
                items: items(),
                binding: CompositeRowBinding {
                    layout: None,
                    ty: "pair".repeat(100),
                    attributes: vec![1, 3, 5],
                    argument_types: Some(vec![ColumnType::Text; 3]),
                },
                bound_type: Some(ColumnType::Array(Box::new(ColumnType::Text))),
            },
            items(),
        ),
        (
            ScalarExpr::Func {
                name: "scalar function".repeat(100),
                binding: None,
                args: vec![text(101)],
                distinct: true,
                order_by: vec![ScalarOrder {
                    expr: text(203),
                    descending: true,
                    nulls: None,
                }],
                order_syntax: FunctionOrderSyntax::Legacy,
                filter: Some(Box::new(text(307))),
            },
            items(),
        ),
    ];
    shapes.extend(compound_shapes());
    shapes
}

fn compound_shapes() -> [(ScalarExpr, Vec<ScalarExpr>); 3] {
    [
        (
            ScalarExpr::WindowCall {
                name: "window function".repeat(100),
                args: vec![text(101)],
                spec: ScalarWindowSpec {
                    definition: Some(1),
                    partition_by: vec![text(203)],
                    order_by: vec![ScalarOrder {
                        expr: text(307),
                        descending: false,
                        nulls: None,
                    }],
                    frame: Some(ScalarWindowFrame {
                        mode: FrameMode::Rows,
                        start: ScalarFrameBound::Preceding(Box::new(text(409))),
                        end: ScalarFrameBound::Following(Box::new(text(503))),
                        between: true,
                        exclusion: FrameExclusion::NoOthers,
                    }),
                },
                filter: Some(Box::new(text(601))),
                modifiers: WindowCallModifiers::default(),
            },
            [101, 203, 307, 409, 503, 601].map(text).into(),
        ),
        (
            ScalarExpr::Case {
                base: Some(Box::new(text(101))),
                when: vec![(text(203), text(307)), (text(409), text(503))],
                else_branch: Some(Box::new(text(601))),
            },
            [101, 203, 307, 409, 503, 601].map(text).into(),
        ),
        (
            ScalarExpr::Collated {
                expr: Box::new(text(101)),
                ty: ColumnType::Text,
                collation: ResultCollation::new(
                    crate::catalog::collations::derivation::Derivation::None,
                    0,
                ),
            },
            vec![text(101)],
        ),
    ]
}

pub(super) fn wide_keyed_calls() -> Vec<(ScalarExpr, usize)> {
    let parameters = |start, end| (start..end).map(ScalarExpr::Param).collect::<Vec<_>>();
    let orders = |start, end| {
        (start..end)
            .map(|index| ScalarOrder {
                expr: ScalarExpr::Param(index),
                descending: index.is_multiple_of(2),
                nulls: None,
            })
            .collect::<Vec<_>>()
    };
    let mut output = vec![(
        ScalarExpr::Func {
            name: "wide_call".into(),
            binding: None,
            args: parameters(0, 512),
            distinct: false,
            order_by: orders(512, 1024),
            order_syntax: FunctionOrderSyntax::Legacy,
            filter: Some(Box::new(ScalarExpr::Param(1024))),
        },
        1025,
    )];
    for offsets in [false, true] {
        output.push((
            ScalarExpr::WindowCall {
                name: "wide_window".into(),
                args: parameters(0, 512),
                spec: ScalarWindowSpec {
                    definition: None,
                    partition_by: parameters(512, 1024),
                    order_by: orders(1024, 1536),
                    frame: offsets.then(|| ScalarWindowFrame {
                        mode: FrameMode::Rows,
                        start: ScalarFrameBound::Preceding(Box::new(ScalarExpr::Param(1536))),
                        end: ScalarFrameBound::Following(Box::new(ScalarExpr::Param(1537))),
                        between: true,
                        exclusion: FrameExclusion::NoOthers,
                    }),
                },
                filter: offsets.then(|| Box::new(ScalarExpr::Param(1538))),
                modifiers: WindowCallModifiers::default(),
            },
            if offsets { 1539 } else { 1536 },
        ));
    }
    output
}
