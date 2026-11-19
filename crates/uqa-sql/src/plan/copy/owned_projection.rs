//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Projection construction transfers scalar and alias producers into their own field records.

use super::{Copying, Result, RewriteField, RewriteMemory};
use crate::plan::{mutable::projections::OwnedProjection, ProjectionPlan};

impl Copying<'_> {
    pub(crate) fn take_projection(
        &mut self,
        projection: OwnedProjection,
    ) -> Result<ProjectionPlan> {
        let expression = projection
            .expression
            .rewrite(&self.control, Ok::<_, crate::SQLError>)?;
        self.fields
            .as_mut()
            .expect("controlled projection fields")
            .reserve(2)?;
        let mut alias_copying = Self::for_rewrite(self.control).expect("projection alias owner");
        let alias = projection
            .alias
            .map(|alias| {
                let (alias, memory) = alias.into_parts();
                let alias = alias_copying.control.finish(alias, memory)?;
                Ok::<_, crate::SQLError>(alias_copying.take(alias))
            })
            .transpose()?;
        let alias_memory = alias_copying.into_rewrite_memory();
        let (expr, memory) = expression.into_rewrite_parts();
        let fields = self.fields.as_mut().expect("controlled projection fields");
        fields
            .push(RewriteField {
                name: "scalar",
                memory: Some(RewriteMemory::Scalar(memory)),
            })
            .expect("projection scalar slot was admitted");
        fields
            .push(RewriteField {
                name: "alias",
                memory: Some(alias_memory),
            })
            .expect("projection alias slot was admitted");
        Ok(ProjectionPlan { expr, alias })
    }
}
