//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use crate::{
    ast::{
        BinaryOp, CompositeRowBinding, Expr, FrameBound, FrameExclusion, FrameMode,
        FunctionCallSyntax, OrderBy, RetainedInputLiteral, WindowCallModifiers, WindowFrame,
        WindowSpec,
    },
    ir::{ConstantValueSource, RetainedConstantValue, RetainedScalarLiteral},
    SQLError,
};
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};
use uqa_core::Value;

#[derive(Debug)]
struct Resource {
    retired: Arc<AtomicUsize>,
    unwind: bool,
}
impl ConstantValueSource for Resource {
    fn read(&self, _: u64) -> Result<Value, SQLError> {
        panic!("retirement must not read a value");
    }
}
impl Drop for Resource {
    fn drop(&mut self) {
        self.retired.fetch_add(1, Ordering::Relaxed);
        assert!(!self.unwind, "injected resource retirement unwind");
    }
}

fn leaf(retired: &Arc<AtomicUsize>, unwind: bool) -> Expr {
    Expr::RetainedLiteral(Box::new(RetainedInputLiteral(RetainedScalarLiteral::new(
        RetainedConstantValue::new(
            Arc::new(Resource {
                retired: retired.clone(),
                unwind,
            }),
            0,
        ),
        None,
        None,
        None,
    ))))
}

fn order(expr: Expr) -> OrderBy {
    OrderBy {
        expr,
        descending: true,
        nulls: None,
    }
}

fn call(args: Vec<Expr>, order_by: Vec<OrderBy>, filter: Option<Expr>) -> Expr {
    Expr::Func {
        name: "f".into(),
        binding: None,
        args,
        distinct: false,
        order_by,
        order_syntax: FunctionCallSyntax::Ordinary,
        filter: filter.map(Box::new),
    }
}

fn window(value: Expr, slot: usize, sibling: Expr) -> Expr {
    let mut spec = WindowSpec {
        raw_definition: None,
        definition: None,
        reference: None,
        partition_by: vec![],
        order_by: vec![],
        frame: None,
    };
    let mut args = vec![];
    let mut filter = None;
    match slot {
        0 => args = vec![value, sibling],
        1 => spec.partition_by = vec![value, sibling],
        2 => spec.order_by = vec![order(value), order(sibling)],
        3 | 4 => {
            let (start, end) = if slot == 3 {
                (value, sibling)
            } else {
                (sibling, value)
            };
            spec.frame = Some(WindowFrame {
                mode: FrameMode::Rows,
                between: true,
                exclusion: FrameExclusion::NoOthers,
                start: FrameBound::Preceding(Box::new(start)),
                end: FrameBound::Following(Box::new(end)),
            });
        }
        5 => {
            args.push(sibling);
            filter = Some(Box::new(value));
        }
        6 => {
            drop(sibling);
            filter = Some(Box::new(value));
        }
        7 => spec.partition_by = vec![Expr::Star, sibling, value],
        8 => spec.order_by = vec![order(Expr::Star), order(sibling), order(value)],
        _ => unreachable!(),
    }
    Expr::WindowCall {
        name: "w".into(),
        args,
        spec: Box::new(spec),
        filter,
        modifiers: WindowCallModifiers::default(),
    }
}

fn surround(value: Expr, slot: usize, leaf: &mut impl FnMut() -> Expr) -> Expr {
    match slot {
        0 => Expr::UnaryMinus(Box::new(value)),
        1 => Expr::Not(Box::new(value)),
        2 => Expr::IsNull {
            expr: Box::new(value),
            negated: true,
        },
        3 => Expr::Cast {
            expr: Box::new(value),
            implicit: false,
            ty: "t".into(),
        },
        4 => Expr::Binary {
            op: BinaryOp::Add,
            lhs: Box::new(value),
            rhs: Box::new(leaf()),
        },
        5 => Expr::Binary {
            op: BinaryOp::Add,
            lhs: Box::new(leaf()),
            rhs: Box::new(value),
        },
        6 => Expr::Between {
            expr: Box::new(value),
            low: Box::new(leaf()),
            high: Box::new(leaf()),
        },
        7 => Expr::Between {
            expr: Box::new(leaf()),
            low: Box::new(value),
            high: Box::new(leaf()),
        },
        8 => Expr::Between {
            expr: Box::new(leaf()),
            low: Box::new(leaf()),
            high: Box::new(value),
        },
        9 => Expr::Array(vec![leaf(), value, Expr::Star]),
        10 => Expr::Row(vec![Expr::Star, value, leaf()]),
        11 => Expr::And(vec![value, leaf()]),
        12 => Expr::Or(vec![leaf(), value]),
        13 => Expr::CompositeRow {
            items: vec![leaf(), value],
            binding: CompositeRowBinding {
                ty: "row".into(),
                attributes: vec![1, 2],
                layout: None,
                argument_types: None,
            },
        },
        14 => Expr::InList {
            expr: Box::new(value),
            list: vec![leaf(), leaf()],
            negated: false,
        },
        15 => Expr::InList {
            expr: Box::new(Expr::Star),
            list: vec![leaf(), value, leaf()],
            negated: true,
        },
        16 => call(vec![value, leaf()], vec![order(leaf())], Some(leaf())),
        17 => call(vec![], vec![order(value), order(leaf())], None),
        18 => call(vec![Expr::Star], vec![order(leaf())], Some(value)),
        19 => call(vec![], vec![], Some(value)),
        20 => Expr::Case {
            base: Some(Box::new(value)),
            when: vec![(leaf(), leaf())],
            else_branch: Some(Box::new(leaf())),
        },
        21 => Expr::Case {
            base: None,
            when: vec![(value, leaf()), (leaf(), leaf())],
            else_branch: None,
        },
        22 => Expr::Case {
            base: Some(Box::new(Expr::Star)),
            when: vec![(leaf(), value)],
            else_branch: None,
        },
        23 => Expr::Case {
            base: Some(Box::new(Expr::Star)),
            when: vec![(leaf(), leaf())],
            else_branch: Some(Box::new(value)),
        },
        24 => Expr::Case {
            base: None,
            when: vec![],
            else_branch: Some(Box::new(value)),
        },
        25..=33 => window(value, slot - 25, leaf()),
        34 => call(vec![Expr::Star, leaf(), value], vec![], None),
        35 => call(
            vec![],
            vec![order(Expr::Star), order(leaf()), order(value)],
            None,
        ),
        _ => unreachable!(),
    }
}

fn exercise(unwind: bool) {
    let retired = Arc::new(AtomicUsize::new(0));
    let mut count = 1;
    let mut value = leaf(&retired, unwind);
    for index in 0..4096 {
        value = surround(value, index % 36, &mut || {
            count += 1;
            leaf(&retired, false)
        });
    }
    let before = retired.load(Ordering::Relaxed);
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let allocations = allocation_counter::measure(|| drop(value));
        assert_eq!(allocations.count_total, 0);
    }));
    assert_eq!(result.is_err(), unwind);
    assert_eq!(retired.load(Ordering::Relaxed), count);
    assert!(count > before);
}

#[test]
fn native_scalar_retirement_releases_every_mixed_child_once() {
    std::thread::Builder::new()
        .stack_size(256 * 1024)
        .spawn(|| exercise(false))
        .unwrap()
        .join()
        .unwrap();
}

#[test]
fn native_scalar_retirement_retires_suspended_parents_after_resource_unwind() {
    std::thread::Builder::new()
        .stack_size(256 * 1024)
        .spawn(|| exercise(true))
        .unwrap()
        .join()
        .unwrap();
}
