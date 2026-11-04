//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! New query containers can consume existing scalar owners without copying their payloads.

use super::{Copying, Result, RewriteField, RewriteMemory, ScalarExpr};
use crate::plan::OwnedScalarExpr;

impl Copying<'_> {
    /// Homogeneous scalar buffers retain each scalar owner directly, without a named field and an enum wrapper for every value.
    pub(crate) fn scalar_values(
        &mut self,
        values: impl ExactSizeIterator<Item = Result<OwnedScalarExpr>>,
    ) -> Result<Vec<ScalarExpr>> {
        self.check()?;
        let budget = self.control.budget().expect("controlled scalar vector");
        let mut owners = self.fields.as_ref().map(|fields| {
            assert!(
                fields.is_empty(),
                "scalar vector constructor owns only indexed fields"
            );
            uqa_core::memory::BudgetedVec::new(budget)
        });
        if let Some(owners) = &mut owners {
            owners.reserve(values.len())?;
        }
        let mut output = uqa_core::memory::BudgetedVec::new(budget);
        output.reserve(values.len())?;
        for value in values {
            let value = value?.rewrite(&self.control, Ok::<_, crate::SQLError>)?;
            output.reserve(1)?;
            if let Some(owners) = &mut owners {
                owners.reserve(1)?;
            }
            let (value, memory) = value.into_rewrite_parts();
            output.push(value).expect("scalar destination was admitted");
            if let Some(owners) = &mut owners {
                owners.push(memory).expect("scalar owner slot was admitted");
            } else {
                self.absorb(Some(memory.expect("controlled scalar").into_reservation()));
            }
        }
        let (output, memory) = output.into_parts();
        self.absorb(Some(memory));
        self.scalar_items = owners;
        Ok(output)
    }

    pub(crate) fn take_scalar(&mut self, scalar: OwnedScalarExpr) -> Result<ScalarExpr> {
        let scalar = scalar.rewrite(&self.control, Ok::<_, crate::SQLError>)?;
        self.fields
            .as_mut()
            .expect("mutable scalar destination")
            .reserve(1)?;
        let (expression, memory) = scalar.into_rewrite_parts();
        self.fields
            .as_mut()
            .expect("mutable scalar destination")
            .push(RewriteField {
                name: "scalar",
                memory: Some(RewriteMemory::Scalar(memory)),
            })
            .expect("scalar owner slot was admitted");
        Ok(expression)
    }
}
