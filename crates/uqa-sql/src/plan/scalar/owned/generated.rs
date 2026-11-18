//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Generated operators admit their containers before moving existing scalar owners.

use super::{OwnedScalarExpr, Result, ScalarExpr, ScalarMemory};
use crate::{
    ast::{BinaryOp, FunctionBinding, FunctionOrderSyntax},
    plan::{
        retirement::Pending,
        scalar::resources::{input::Work, Control, Lowering},
    },
    schema::retention::CatalogRetentionError,
};
use uqa_core::memory::{Produced, ProductionControl};

impl OwnedScalarExpr {
    /// Construct CASE from independent scalar owners, admitting each container before moving its children.
    pub fn case(
        base: Option<Self>,
        branches: impl ExactSizeIterator<Item = (Self, Self)>,
        fallback: Option<Self>,
        control: &ProductionControl<'_>,
    ) -> Result<Self> {
        crate::catalog::work::inline(Self::case_from(base, branches, fallback, control, &mut ()))
    }

    pub(crate) async fn case_from<W: Work>(
        base: Option<Self>,
        branches: impl ExactSizeIterator<Item = (Self, Self)>,
        fallback: Option<Self>,
        control: &ProductionControl<'_>,
        work: &mut W,
    ) -> std::result::Result<Self, W::Error>
    where
        W::Error: From<CatalogRetentionError>,
    {
        control.check()?;
        let mut lowering = owned_lowering(control);
        let mut output = Pending::new(ScalarExpr::Case {
            base: None,
            when: Vec::new(),
            else_branch: None,
        });
        let ScalarExpr::Case {
            base: destination,
            when,
            else_branch,
        } = &mut *output
        else {
            unreachable!()
        };
        if let Some(base) = base {
            lowering.checkpoint(work).await?;
            lowering.admit_box::<ScalarExpr>()?;
            *destination = Some(Box::new(Self::attach_child(&mut lowering, base)?));
        }
        lowering.checkpoint(work).await?;
        *when = lowering.vector(branches.len())?;
        for (condition, value) in branches {
            lowering.checkpoint(work).await?;
            let condition = Pending::new(Self::attach_child(&mut lowering, condition)?);
            let value = Self::attach_child(&mut lowering, value)?;
            when.push((condition.take(), value));
        }
        if let Some(fallback) = fallback {
            lowering.checkpoint(work).await?;
            lowering.admit_box::<ScalarExpr>()?;
            *else_branch = Some(Box::new(Self::attach_child(&mut lowering, fallback)?));
        }
        Ok(finish(output.take(), lowering, control)?)
    }

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
        crate::catalog::work::inline(Self::array_from(children, control, &mut ()))
    }

    pub(crate) async fn array_from<W: Work>(
        children: impl ExactSizeIterator<Item = Self>,
        control: &ProductionControl<'_>,
        work: &mut W,
    ) -> std::result::Result<Self, W::Error>
    where
        W::Error: From<CatalogRetentionError>,
    {
        control.check()?;
        let mut lowering = owned_lowering(control);
        let items = children_from(&mut lowering, children, work).await?;
        Ok(finish(ScalarExpr::Array(items.take()), lowering, control)?)
    }

    /// Construct a ROW from independently owned fields, admitting its vector and ownership slots before moving children.
    pub fn row(
        children: impl ExactSizeIterator<Item = Self>,
        control: &ProductionControl<'_>,
    ) -> Result<Self> {
        crate::catalog::work::inline(Self::row_from(children, control, &mut ()))
    }

    pub(crate) async fn row_from<W: Work>(
        children: impl ExactSizeIterator<Item = Self>,
        control: &ProductionControl<'_>,
        work: &mut W,
    ) -> std::result::Result<Self, W::Error>
    where
        W::Error: From<CatalogRetentionError>,
    {
        control.check()?;
        let mut lowering = owned_lowering(control);
        let items = children_from(&mut lowering, children, work).await?;
        Ok(finish(ScalarExpr::Row(items.take()), lowering, control)?)
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
        lowering.admit_box::<ScalarExpr>()?;
        let lhs = Pending::new(Box::new(Self::attach_child(&mut lowering, left)?));
        lowering.admit_box::<ScalarExpr>()?;
        let rhs = Box::new(Self::attach_child(&mut lowering, right)?);
        finish(
            ScalarExpr::Binary {
                op,
                lhs: lhs.take(),
                rhs,
            },
            lowering,
            control,
        )
    }

    /// Construct an undecorated selected call from admitted metadata and movable arguments. Overload selection and argument evaluation remain with the caller.
    pub fn call(
        name: Produced<String>,
        binding: Option<Produced<FunctionBinding>>,
        arguments: impl ExactSizeIterator<Item = Self>,
        control: &ProductionControl<'_>,
    ) -> Result<Self> {
        crate::catalog::work::inline(Self::call_from(name, binding, arguments, control, &mut ()))
    }

    pub(crate) async fn call_from<W: Work>(
        name: Produced<String>,
        binding: Option<Produced<FunctionBinding>>,
        arguments: impl ExactSizeIterator<Item = Self>,
        control: &ProductionControl<'_>,
        work: &mut W,
    ) -> std::result::Result<Self, W::Error>
    where
        W::Error: From<CatalogRetentionError>,
    {
        control.check()?;
        let mut lowering = owned_lowering(control);
        let mut output = call_fields(&mut lowering, name, binding);
        let arguments = children_from(&mut lowering, arguments, work).await?;
        let ScalarExpr::Func { args, .. } = &mut *output else {
            unreachable!()
        };
        *args = arguments.take();
        Ok(finish(output.take(), lowering, control)?)
    }
}

pub(super) fn call_fields(
    lowering: &mut Lowering<'_>,
    name: Produced<String>,
    binding: Option<Produced<FunctionBinding>>,
) -> Pending<ScalarExpr> {
    Pending::new(ScalarExpr::Func {
        name: lowering.take_input(name),
        binding: binding.map(|binding| lowering.take_input(binding)),
        args: Vec::new(),
        distinct: false,
        order_by: Vec::new(),
        order_syntax: FunctionOrderSyntax::Ordinary,
        filter: None,
    })
}

pub(super) async fn children_from<W: Work>(
    lowering: &mut Lowering<'_>,
    children: impl ExactSizeIterator<Item = OwnedScalarExpr>,
    work: &mut W,
) -> std::result::Result<Pending<Vec<ScalarExpr>>, W::Error>
where
    W::Error: From<CatalogRetentionError>,
{
    lowering.checkpoint(work).await?;
    let mut items = Pending::new(lowering.vector(children.len())?);
    for child in children {
        lowering.checkpoint(work).await?;
        items.push(OwnedScalarExpr::attach_child(lowering, child)?);
    }
    Ok(items)
}

pub(super) fn owned_lowering<'a>(control: &ProductionControl<'a>) -> Lowering<'a> {
    Lowering {
        control: control.budget().map(|_| Control::from_production(*control)),
        owned_children: control
            .budget()
            .map(|budget| ScalarMemory::new(budget.empty_reservation())),
    }
}

pub(super) fn finish(
    expression: ScalarExpr,
    lowering: Lowering<'_>,
    control: &ProductionControl<'_>,
) -> Result<OwnedScalarExpr> {
    let output = OwnedScalarExpr {
        expression,
        memory: lowering.control.map(|control| {
            ScalarMemory::from_parts(
                control.memory,
                lowering
                    .owned_children
                    .expect("mutable generated expression"),
            )
        }),
    };
    control.check()?;
    Ok(output)
}
