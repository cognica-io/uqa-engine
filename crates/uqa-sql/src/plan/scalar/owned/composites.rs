//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Composite constant projection keeps the replacement datum and its original provenance under one literal constructor owner.

use super::{OwnedScalarExpr, Result, ScalarExpr};
use crate::expr::composites::CompositeConstantSource;
use uqa_core::{
    memory::{Produced, ProductionControl},
    Value,
};

impl OwnedScalarExpr {
    pub(crate) fn replace_composite_literal(
        self,
        value: Produced<Value>,
        source: Produced<CompositeConstantSource>,
        control: &ProductionControl<'_>,
    ) -> Result<Self> {
        self.assert_owner(control);
        let ScalarExpr::TypedLiteral {
            ty,
            bound_type,
            collation_oid,
            parameter_index,
            ..
        } = &self.expression
        else {
            panic!("composite projection requires a typed literal");
        };
        let mut output = Self::typed_literal(
            value,
            control.copy_text(ty)?,
            bound_type
                .as_ref()
                .map(|ty| ty.clone_with_control(control))
                .transpose()?,
            *collation_oid,
            *parameter_index,
            control,
        )?;
        let header = control.reserve(size_of::<CompositeConstantSource>())?;
        let (source, memory) = source.into_parts();
        let ScalarExpr::TypedLiteral {
            composite_source, ..
        } = &mut output.expression
        else {
            unreachable!();
        };
        *composite_source = Some(Box::new(source));
        if let Some(owner) = &mut output.memory {
            owner.direct.absorb(
                control
                    .combine(memory, header)
                    .expect("admitted composite source"),
            );
        }
        drop(self);
        control.check()?;
        Ok(output)
    }
}
