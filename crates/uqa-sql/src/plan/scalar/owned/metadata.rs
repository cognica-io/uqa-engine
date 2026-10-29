//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Analyzed metadata changes retain child payloads and admit only their new constructors.

use super::{OwnedScalarExpr, Result, ScalarExpr, ScalarMemory};
use crate::{catalog::collations::CollationSelection, ir::ResultCollation, ColumnType};
use uqa_core::{
    memory::{BudgetedVec, Produced, ProductionControl},
    Value,
};

impl OwnedScalarExpr {
    /// Wrap the surviving expression in the exact type and collation selected during ordered analysis.
    pub fn collated(
        child: Self,
        ty: Produced<ColumnType>,
        collation: ResultCollation,
        control: &ProductionControl<'_>,
    ) -> Result<Self> {
        child.assert_owner(control);
        control.check()?;
        let mut memory = control.empty_reservation().map(|direct| ScalarMemory {
            children: BudgetedVec::new(direct.budget()),
            direct,
        });
        if let Some(memory) = &mut memory {
            memory.children.reserve(1)?;
        }
        let boxed = control.reserve(size_of::<ScalarExpr>())?;
        let (ty, type_memory) = ty.into_parts();
        let Self {
            expression,
            memory: child_memory,
        } = child;
        let mut output = Self {
            expression: ScalarExpr::Literal(Value::Null),
            memory,
        };
        output.expression = ScalarExpr::Collated {
            expr: Box::new(expression),
            ty,
            collation,
        };
        if let Some(memory) = &mut output.memory {
            memory
                .children
                .push(child_memory)
                .expect("admitted collation child");
            memory.direct.absorb(
                control
                    .combine(boxed, type_memory)
                    .expect("admitted collation metadata"),
            );
        }
        control.check()?;
        Ok(output)
    }

    /// Set a selected call's inline collation identity without rebuilding its allocating metadata.
    pub fn set_call_input_collation(
        mut self,
        collation: Option<CollationSelection>,
        control: &ProductionControl<'_>,
    ) -> Result<Self> {
        self.assert_owner(control);
        control.check()?;
        let ScalarExpr::Func {
            binding: Some(binding),
            ..
        } = &mut self.expression
        else {
            panic!("input collation requires a selected call")
        };
        binding.input_collation = collation;
        Ok(self)
    }

    /// Set a typed literal's inline collation identity while retaining its exact value and type owners.
    pub fn set_literal_collation(
        mut self,
        oid: u32,
        control: &ProductionControl<'_>,
    ) -> Result<Self> {
        self.assert_owner(control);
        control.check()?;
        let ScalarExpr::TypedLiteral { collation_oid, .. } = &mut self.expression else {
            panic!("literal collation requires a typed literal")
        };
        *collation_oid = Some(oid);
        Ok(self)
    }
}
