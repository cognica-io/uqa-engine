//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Scalar containers admit their own allocation without merging the independently movable children.

use super::{OwnedScalarExpr, Result, ScalarExpr, ScalarMemory};
use crate::plan::scalar::resources::{Control, Lowering};
use uqa_core::{
    memory::{BudgetedVec, ProductionControl},
    Value,
};

impl OwnedScalarExpr {
    /// Construct an unbound typed NULL, admitting the declared type name before copying it. This storage constructor performs no type resolution or value evaluation.
    pub fn typed_null(ty: &str, control: &ProductionControl<'_>) -> Result<Self> {
        let (ty, memory) = control.copy_text(ty)?.into_parts();
        let output = Self {
            expression: ScalarExpr::TypedLiteral {
                value: Value::Null,
                ty,
                bound_type: None,
                collation_oid: None,
                composite_source: None,
                parameter_index: None,
            },
            memory: memory.map(|direct| ScalarMemory {
                children: BudgetedVec::new(direct.budget()),
                direct,
            }),
        };
        control.check()?;
        Ok(output)
    }

    /// Build AND or OR from already owned children. The destination vector and ownership metadata reserve before allocation; input payloads move without copying or changing their allowance.
    pub fn boolean(
        children: impl ExactSizeIterator<Item = Self>,
        conjunction: bool,
        control: &ProductionControl<'_>,
    ) -> Result<Self> {
        control.check()?;
        let mut lowering = Lowering {
            control: control.budget().map(|_| Control::from_production(*control)),
            owned_children: control.budget().map(BudgetedVec::new),
        };
        let items = lowering.map(children, Self::attach_child)?;
        let expression = if conjunction {
            ScalarExpr::And(items)
        } else {
            ScalarExpr::Or(items)
        };
        let output = Self {
            expression,
            memory: lowering.control.map(|control| ScalarMemory {
                direct: control.memory,
                children: lowering
                    .owned_children
                    .expect("mutable scalar construction"),
            }),
        };
        control.check()?;
        Ok(output)
    }

    pub(in crate::plan::scalar) fn attach_child(
        lowering: &mut Lowering<'_>,
        child: Self,
    ) -> Result<ScalarExpr> {
        let control = lowering
            .control
            .as_ref()
            .map_or_else(ProductionControl::uncontrolled, |control| {
                control.production
            });
        child.assert_owner(&control);
        control.check()?;
        if let Some(children) = &mut lowering.owned_children {
            children.reserve(1)?;
        }
        let Self { expression, memory } = child;
        if let Some(children) = &mut lowering.owned_children {
            children
                .push(memory)
                .expect("scalar child slot was admitted");
        }
        Ok(expression)
    }
}
