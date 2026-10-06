//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Selected IN-list structure shared by analyzed scalars and their retained syntax. Array candidates precede individual comparisons; each individual comparison owns its left operand.

use crate::ast::{BinaryOp, Expr, FunctionBinding, FunctionDispatch, FunctionOrderSyntax};
use crate::{SQLError, ScalarExpr};
use uqa_core::{
    memory::{MemoryReservation, Produced, ProductionControl, ProductionVec},
    Value,
};

/// Positions combined into the scalar-array comparison. An empty list selects only individual comparisons.
#[derive(Debug, Clone, PartialEq, Default)]
pub(crate) struct MembershipShape {
    pub(crate) array_items: Vec<usize>,
}

#[derive(PartialEq, Eq, Hash)]
enum RowReference {
    Column(crate::schema::ColumnIdentity),
    Position(usize),
    Internal(crate::ast::InternalColumnRef),
    WholeRow(String),
}

fn row_references(expression: &ScalarExpr) -> std::collections::HashSet<RowReference> {
    let mut references = std::collections::HashSet::new();
    expression.visit(&mut |node| {
        let reference = match node {
            ScalarExpr::Column(column) => Some(RowReference::Column(
                crate::schema::ColumnIdentity::unqualified(column.clone()),
            )),
            ScalarExpr::QualifiedColumn { qualifier, column } => Some(RowReference::Column(
                crate::schema::ColumnIdentity::qualified(qualifier.clone(), column.clone()),
            )),
            ScalarExpr::Position(position) => Some(RowReference::Position(*position)),
            ScalarExpr::InternalColumn(column) => Some(RowReference::Internal(*column)),
            ScalarExpr::QualifiedStar(qualifier) => Some(RowReference::WholeRow(qualifier.clone())),
            _ => None,
        };
        if let Some(reference) = reference {
            references.insert(reference);
        }
    });
    references
}

/// Recover the grouping chosen by analysis without selecting types again. The original positions remain in written order within each group.
pub(crate) fn stored_shape(original: &ScalarExpr, bound: &ScalarExpr) -> Option<MembershipShape> {
    let ScalarExpr::InList { list, .. } = original else {
        return None;
    };
    let first = match bound {
        ScalarExpr::And(items) | ScalarExpr::Or(items) => items.first()?,
        expression => expression,
    };
    let array = matches!(first, ScalarExpr::Func { binding: Some(binding), .. } if matches!(binding.dispatch, Some(FunctionDispatch::AnyOperator | FunctionDispatch::AllOperator)));
    if !array && !matches!(first, ScalarExpr::Binary { .. }) {
        return None;
    }
    Some(MembershipShape {
        array_items: if array {
            let mut array_queries = std::collections::BTreeSet::new();
            let mut array_rows = std::collections::HashSet::new();
            if let ScalarExpr::Func { args, .. } = first {
                crate::semantics::collect_subquery_ids(&args[1], &mut array_queries);
                array_rows = row_references(&args[1]);
            }
            list.iter()
                .enumerate()
                .filter_map(|(index, item)| {
                    let mut queries = std::collections::BTreeSet::new();
                    crate::semantics::collect_subquery_ids(item, &mut queries);
                    (row_references(item).is_subset(&array_rows)
                        && queries.is_subset(&array_queries))
                    .then_some(index)
                })
                .collect()
        } else {
            Vec::new()
        },
    })
}

pub(crate) trait MembershipSyntax: Sized {
    fn copy(&self, control: &ProductionControl<'_>) -> Result<Produced<Self>, SQLError>;
    fn array(items: Vec<Self>) -> Self;
    fn literal(value: Value) -> Self;
    fn binary(op: BinaryOp, lhs: Box<Self>, rhs: Box<Self>) -> Self;
    fn boolean(negated: bool, items: Vec<Self>) -> Self;
    fn quantified(name: String, binding: FunctionBinding, args: Vec<Self>) -> Self;
}

macro_rules! syntax {
    ($ty:ty, $copy:expr) => {
        impl MembershipSyntax for $ty {
            fn copy(&self, control: &ProductionControl<'_>) -> Result<Produced<Self>, SQLError> {
                ($copy)(self, control)
            }
            fn array(items: Vec<Self>) -> Self {
                Self::Array(items)
            }
            fn literal(value: Value) -> Self {
                Self::Literal(value)
            }
            fn binary(op: BinaryOp, lhs: Box<Self>, rhs: Box<Self>) -> Self {
                Self::Binary { op, lhs, rhs }
            }
            fn boolean(negated: bool, items: Vec<Self>) -> Self {
                if negated {
                    Self::And(items)
                } else {
                    Self::Or(items)
                }
            }
            fn quantified(name: String, binding: FunctionBinding, args: Vec<Self>) -> Self {
                Self::Func {
                    name,
                    binding: Some(binding),
                    args,
                    distinct: false,
                    order_by: Vec::new(),
                    order_syntax: FunctionOrderSyntax::default(),
                    filter: None,
                }
            }
        }
    };
}
syntax!(
    ScalarExpr,
    |value: &ScalarExpr, control: &ProductionControl<'_>| Ok(value.clone_with_control(control)?)
);
syntax!(Expr, |value: &Expr, control: &ProductionControl<'_>| {
    assert!(
        control.budget().is_none(),
        "stored syntax is rewritten by its ordinary catalog owner"
    );
    Ok(control.finish(value.clone(), None)?)
});

/// The caller retains admission for the moved input nodes; the returned owner admits all newly allocated structure and copies.
pub(crate) fn rewrite<T: MembershipSyntax>(
    value: Box<T>,
    list: Vec<T>,
    negated: bool,
    shape: &MembershipShape,
    control: &ProductionControl<'_>,
) -> Result<Produced<T>, SQLError> {
    let mut build = Builder {
        control: *control,
        memory: control.empty_reservation(),
    };
    let mut array = ProductionVec::new(*control);
    let mut individual = ProductionVec::new(*control);
    let mut positions = shape.array_items.iter().peekable();
    for (index, item) in list.into_iter().enumerate() {
        let target = if positions.peek() == Some(&&index) {
            positions.next();
            &mut array
        } else {
            &mut individual
        };
        target.push_produced(control.finish(item, control.empty_reservation())?)?;
    }
    let array = build.retain(array.finish()?);
    let individual = build.retain(individual.finish()?);
    let count = usize::from(!array.is_empty()) + individual.len();
    let mut left = Some(value);
    let mut comparisons = ProductionVec::new(*control);
    if !array.is_empty() {
        let mut args = ProductionVec::new(*control);
        let lhs = build.left(&mut left, count > 1)?;
        args.push_produced(control.finish(*lhs, control.empty_reservation())?)?;
        args.push_produced(control.finish(T::array(array), control.empty_reservation())?)?;
        let operator = build.retain(control.copy_text(if negated { "<>" } else { "=" })?);
        args.push_produced(control.finish(
            T::literal(Value::Str(operator)),
            control.empty_reservation(),
        )?)?;
        let args = build.retain(args.finish()?);
        let dispatch = if negated {
            FunctionDispatch::AllOperator
        } else {
            FunctionDispatch::AnyOperator
        };
        let binding = build.retain(FunctionBinding::dispatched_with_control(dispatch, control)?);
        let name =
            build.retain(control.copy_text(if negated { "__all_op" } else { "__any_op" })?);
        comparisons.push_produced(control.finish(
            T::quantified(name, binding, args),
            control.empty_reservation(),
        )?)?;
    }
    let length = individual.len();
    for (index, item) in individual.into_iter().enumerate() {
        let lhs = build.left(&mut left, index + 1 < length)?;
        build.memory = control.combine(build.memory.take(), control.reserve(size_of::<T>())?);
        let comparison = T::binary(
            if negated {
                BinaryOp::NotEqual
            } else {
                BinaryOp::Equal
            },
            lhs,
            Box::new(item),
        );
        comparisons.push_produced(control.finish(comparison, control.empty_reservation())?)?;
    }
    let mut comparisons = build.retain(comparisons.finish()?);
    let expression = if comparisons.len() == 1 {
        comparisons.pop().expect("one comparison")
    } else {
        T::boolean(negated, comparisons)
    };
    Ok(control.finish(expression, build.memory)?)
}

struct Builder<'a> {
    control: ProductionControl<'a>,
    memory: Option<MemoryReservation>,
}

impl Builder<'_> {
    fn retain<T>(&mut self, produced: Produced<T>) -> T {
        let (value, memory) = produced.into_parts();
        self.memory = self.control.combine(self.memory.take(), memory);
        value
    }

    fn left<T: MembershipSyntax>(
        &mut self,
        value: &mut Option<Box<T>>,
        copy: bool,
    ) -> Result<Box<T>, SQLError> {
        if !copy {
            return Ok(value
                .take()
                .expect("last comparison moves the left operand"));
        }
        self.memory = self
            .control
            .combine(self.memory.take(), self.control.reserve(size_of::<T>())?);
        let copied = value.as_ref().expect("left operand").copy(&self.control)?;
        Ok(Box::new(self.retain(copied)))
    }
}
