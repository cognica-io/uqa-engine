//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Scalar containers admit their own allocation without merging the independently movable children.

use super::{
    generated::{children_from, finish, owned_lowering},
    OwnedScalarExpr, Result, ScalarExpr, ScalarMemory,
};
use crate::{
    plan::scalar::resources::{input::Work, Lowering},
    schema::retention::CatalogRetentionError,
};
use uqa_core::{memory::ProductionControl, Value};

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
                children: Vec::new(),
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
        crate::catalog::work::inline(Self::boolean_from(children, conjunction, control, &mut ()))
    }

    pub(crate) async fn boolean_from<W: Work>(
        children: impl ExactSizeIterator<Item = Self>,
        conjunction: bool,
        control: &ProductionControl<'_>,
        work: &mut W,
    ) -> std::result::Result<Self, W::Error>
    where
        W::Error: From<CatalogRetentionError>,
    {
        control.check()?;
        let mut lowering = owned_lowering(control);
        let items = children_from(&mut lowering, children, work).await?;
        let expression = if conjunction {
            ScalarExpr::And(items.take())
        } else {
            ScalarExpr::Or(items.take())
        };
        Ok(finish(expression, lowering, control)?)
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
        let (expression, memory) = child.into_rewrite_parts();
        if let Some(children) = &mut lowering.owned_children {
            children
                .push(memory)
                .expect("scalar child slot was admitted");
        }
        Ok(expression)
    }
}
