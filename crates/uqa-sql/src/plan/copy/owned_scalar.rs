//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! New query containers can consume existing scalar owners without copying their payloads.

use super::{Copying, Result, RewriteField, RewriteMemory, ScalarExpr};
use crate::plan::OwnedScalarExpr;

impl Copying<'_> {
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
                memory: Some(RewriteMemory::Scalar(
                    memory.expect("controlled scalar source"),
                )),
            })
            .expect("scalar owner slot was admitted");
        Ok(expression)
    }
}
