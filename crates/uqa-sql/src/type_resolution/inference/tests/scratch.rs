//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

fn membership(width: usize) -> ScalarExpr {
    ScalarExpr::InList {
        expr: Box::new(ScalarExpr::Literal(Value::Int(1))),
        list: vec![ScalarExpr::Literal(Value::Int(1)); width],
        negated: false,
    }
}

fn conditional(width: usize) -> ScalarExpr {
    ScalarExpr::Case {
        base: None,
        when: vec![
            (
                ScalarExpr::Literal(Value::Bool(true)),
                ScalarExpr::Literal(Value::Int(1)),
            );
            width
        ],
        else_branch: None,
    }
}

fn reject_before_large_scratch(expression: &ScalarExpr) {
    let schema = RowSchema::default();
    let budget = MemoryBudget::new(128);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let mut result = None;
    let allocation = allocation_counter::measure(|| {
        result = Some(infer(expression, &schema, &[], &control));
    });
    assert!(allocation.bytes_total < 32 * 1024, "{allocation:?}");
    assert_eq!(result.unwrap().unwrap_err().sqlstate(), Some("53200"));
    assert_eq!(budget.used(), 0);
}

#[test]
fn membership_type_scratch_is_admitted_before_wide_input_inference() {
    reject_before_large_scratch(&membership(8192));
}

#[test]
fn conditional_type_scratch_is_admitted_before_wide_branch_inference() {
    reject_before_large_scratch(&conditional(8192));
}

#[test]
fn membership_reference_scratch_requires_its_own_admission() {
    let expression = membership(8192);
    let schema = RowSchema::default();
    let capacity = 8192 * size_of::<Option<Produced<ColumnType>>>();
    let budget = MemoryBudget::new(capacity);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let mut result = None;
    let allocation = allocation_counter::measure(|| {
        result = Some(infer(&expression, &schema, &[], &control));
    });
    assert!(
        allocation.bytes_total <= (capacity + 4096) as u64,
        "{allocation:?}"
    );
    assert_eq!(result.unwrap().unwrap_err().sqlstate(), Some("53200"));
    assert_eq!(budget.peak(), capacity);
    assert_eq!(budget.used(), 0);
}

#[test]
fn inference_scratch_releases_capacity_before_publishing_the_selected_type() {
    let schema = RowSchema::default();
    let ty = ColumnType::Array(Box::new(ColumnType::Integer));
    let params = [SQLParam::typed_scalar(Value::Null, ty.clone())];
    let expressions = [
        (
            ScalarExpr::InList {
                expr: Box::new(ScalarExpr::Param(1)),
                list: vec![ScalarExpr::Param(1); 32],
                negated: false,
            },
            ColumnType::Boolean,
        ),
        (
            ScalarExpr::Case {
                base: None,
                when: vec![(ScalarExpr::Literal(Value::Bool(true)), ScalarExpr::Param(1)); 32],
                else_branch: Some(Box::new(ScalarExpr::Param(1))),
            },
            ty,
        ),
    ];
    for (expression, expected) in expressions {
        let parent = MemoryBudget::new(128 * 1024);
        let sibling = parent.reserve(8192).unwrap();
        let budget = parent.child(64 * 1024);
        let token = CancellationToken::new();
        let control = ProductionControl::new(&budget, &token, &token);
        let result = infer(&expression, &schema, &params, &control)
            .unwrap()
            .unwrap();
        assert_eq!(*result, expected);
        assert!(budget.peak() >= 32 * size_of::<Option<Produced<ColumnType>>>());
        assert_eq!(budget.used(), result.reserved_bytes());
        assert_eq!(parent.used(), sibling.bytes() + result.reserved_bytes());
        drop(result);
        assert_eq!(budget.used(), 0);
        assert_eq!(parent.used(), sibling.bytes());
        let exact = parent.child(budget.peak());
        let result = infer(
            &expression,
            &schema,
            &params,
            &ProductionControl::new(&exact, &token, &token),
        )
        .unwrap()
        .unwrap();
        assert_eq!(*result, expected);
        drop(result);
        assert_eq!(parent.used(), sibling.bytes());
    }
}

enum Action<'a> {
    Cancel(&'a CancellationToken),
    Panic,
}

struct InterruptedTypes<'a> {
    ty: ColumnType,
    budget: &'a MemoryBudget,
    action: Action<'a>,
    calls: std::cell::Cell<usize>,
}

impl crate::schema::ScalarTypeSchema for InterruptedTypes<'_> {
    fn observed_expression_type(&self, expression: &ScalarExpr) -> Option<Option<&ColumnType>> {
        let ScalarExpr::Position(position) = expression else {
            return None;
        };
        self.calls.set(self.calls.get() + 1);
        if *position == 3 {
            assert!(
                self.budget.used()
                    >= 4 * size_of::<Option<Produced<ColumnType>>>() + size_of::<ColumnType>()
            );
            match self.action {
                Action::Cancel(token) => token.cancel(),
                Action::Panic => {
                    panic!("type observation unwinds after partial scratch production")
                }
            }
        }
        Some(Some(&self.ty))
    }

    fn has_unqualified_column(&self, _: &str) -> bool {
        unreachable!()
    }
    fn column_is_ambiguous(&self, _: &str) -> bool {
        unreachable!()
    }
    fn type_of(&self, _: &str) -> Option<&ColumnType> {
        unreachable!()
    }
    fn column_type(&self, _: usize) -> Option<&ColumnType> {
        unreachable!()
    }
    fn internal_type(&self, _: crate::ast::InternalColumnRef) -> Option<&ColumnType> {
        unreachable!()
    }
    fn has_qualifier(&self, _: &str) -> bool {
        unreachable!()
    }
    fn has_qualified_column(&self, _: &str, _: &str) -> bool {
        unreachable!()
    }
    fn qualified_column_is_ambiguous(&self, _: &str, _: &str) -> bool {
        unreachable!()
    }
    fn qualified_type(&self, _: &str, _: &str) -> Option<&ColumnType> {
        unreachable!()
    }
    fn columns_are_open(&self, _: Option<&str>) -> bool {
        unreachable!()
    }
    fn physical_schema(&self) -> Option<&RowSchema> {
        None
    }
}

#[test]
fn inference_scratch_and_partial_types_release_on_both_cancellations_and_unwind() {
    let expressions = [
        ScalarExpr::InList {
            expr: Box::new(ScalarExpr::Position(0)),
            list: (1..5).map(ScalarExpr::Position).collect(),
            negated: false,
        },
        ScalarExpr::Case {
            base: None,
            when: (0..4)
                .map(|position| {
                    (
                        ScalarExpr::Literal(Value::Bool(true)),
                        ScalarExpr::Position(position),
                    )
                })
                .collect(),
            else_branch: None,
        },
    ];
    for expression in expressions {
        for mode in 0..3 {
            let parent = MemoryBudget::new(128 * 1024);
            let sibling = parent.reserve(8192).unwrap();
            let budget = parent.child(64 * 1024);
            let original = CancellationToken::new();
            let invoking = CancellationToken::new();
            let schema = InterruptedTypes {
                ty: ColumnType::Array(Box::new(ColumnType::Integer)),
                budget: &budget,
                action: match mode {
                    0 => Action::Cancel(&original),
                    1 => Action::Cancel(&invoking),
                    _ => Action::Panic,
                },
                calls: std::cell::Cell::new(0),
            };
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                scalar_type_inner_with_control(
                    &expression,
                    &schema,
                    &[],
                    None,
                    &ProductionControl::new(&budget, &original, &invoking),
                )
            }));
            if mode == 2 {
                assert!(result.is_err());
            } else {
                assert_eq!(result.unwrap().unwrap_err().sqlstate(), Some("57014"));
            }
            assert_eq!(schema.calls.get(), 4);
            assert_eq!(budget.used(), 0);
            assert_eq!(parent.used(), sibling.bytes());
        }
    }
}
