//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Generated operators admit their containers before moving existing scalar owners.

use super::{OwnedScalarExpr, Result, ScalarExpr, ScalarMemory};
use crate::{
    ast::{BinaryOp, FunctionBinding, FunctionOrderSyntax},
    plan::scalar::resources::{Control, Lowering},
};
use uqa_core::memory::{BudgetedVec, Produced, ProductionControl};

impl OwnedScalarExpr {
    /// Replace a named row constructor's resolved type while moving its scalar fields into admitted destination slots. Binding metadata is copied through its existing owner before the old constructor is retired.
    pub fn replace_composite_type(
        mut self,
        bound_type: Option<Produced<crate::ColumnType>>,
        control: &ProductionControl<'_>,
    ) -> Result<Self> {
        self.assert_owner(control);
        control.check()?;
        let ScalarExpr::CompositeRow { items, binding, .. } = &self.expression else {
            panic!("composite type replacement requires a named row constructor");
        };
        let count = items.len();
        let mut lowering = owned_lowering(control);
        let binding =
            lowering.composite_binding(crate::plan::scalar::source::Source::Borrowed(binding))?;
        let (bound_type, memory) = match bound_type {
            Some(bound_type) => {
                let (bound_type, memory) = bound_type.into_parts();
                (Some(bound_type), memory)
            }
            None => (None, control.empty_reservation()),
        };
        let memory = control.combine(control.empty_reservation(), memory);
        if let Some(owner) = &mut lowering.control {
            owner
                .memory
                .absorb(memory.expect("admitted composite type"));
        }
        let items = lowering.map(0..count, |lowering, index| {
            Self::attach_child(lowering, self.take_child(index))
        })?;
        let output = finish(
            ScalarExpr::CompositeRow {
                items,
                binding,
                bound_type,
            },
            lowering,
            control,
        )?;
        drop(self);
        Ok(output)
    }

    /// Construct an array expression from independently owned scalar elements.
    pub fn array(
        children: impl ExactSizeIterator<Item = Self>,
        control: &ProductionControl<'_>,
    ) -> Result<Self> {
        control.check()?;
        let mut lowering = owned_lowering(control);
        let items = lowering.map(children, Self::attach_child)?;
        finish(ScalarExpr::Array(items), lowering, control)
    }

    /// Construct a binary operator without copying either operand. The two boxes and mutable child slots are admitted before allocation.
    pub fn binary(
        op: BinaryOp,
        left: Self,
        right: Self,
        control: &ProductionControl<'_>,
    ) -> Result<Self> {
        control.check()?;
        let mut lowering = owned_lowering(control);
        let lhs = lowering.boxed(|lowering| Self::attach_child(lowering, left))?;
        let rhs = lowering.boxed(|lowering| Self::attach_child(lowering, right))?;
        finish(ScalarExpr::Binary { op, lhs, rhs }, lowering, control)
    }

    /// Construct an undecorated selected call from admitted metadata and movable arguments. Overload selection and argument evaluation remain with the caller.
    pub fn call(
        name: Produced<String>,
        binding: Option<Produced<FunctionBinding>>,
        arguments: impl ExactSizeIterator<Item = Self>,
        control: &ProductionControl<'_>,
    ) -> Result<Self> {
        control.check()?;
        let mut lowering = owned_lowering(control);
        let (name, name_memory) = name.into_parts();
        let (binding, binding_memory) = match binding {
            Some(binding) => {
                let (binding, memory) = binding.into_parts();
                (Some(binding), memory)
            }
            None => (None, control.empty_reservation()),
        };
        let metadata = control.combine(name_memory, binding_memory);
        if let Some(owner) = &mut lowering.control {
            owner
                .memory
                .absorb(metadata.expect("admitted call metadata"));
        }
        let args = lowering.map(arguments, Self::attach_child)?;
        finish(
            ScalarExpr::Func {
                name,
                binding,
                args,
                distinct: false,
                order_by: Vec::new(),
                order_syntax: FunctionOrderSyntax::Ordinary,
                filter: None,
            },
            lowering,
            control,
        )
    }
}

fn owned_lowering<'a>(control: &ProductionControl<'a>) -> Lowering<'a> {
    Lowering {
        control: control.budget().map(|_| Control::from_production(*control)),
        owned_children: control.budget().map(BudgetedVec::new),
    }
}

fn finish(
    expression: ScalarExpr,
    lowering: Lowering<'_>,
    control: &ProductionControl<'_>,
) -> Result<OwnedScalarExpr> {
    let output = OwnedScalarExpr {
        expression,
        memory: lowering.control.map(|control| ScalarMemory {
            direct: control.memory,
            children: lowering
                .owned_children
                .expect("mutable generated expression"),
        }),
    };
    control.check()?;
    Ok(output)
}
