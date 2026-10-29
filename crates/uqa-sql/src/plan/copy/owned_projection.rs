//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Projection construction transfers scalar and alias producers into their own field records.

use super::{Copying, Result};
use crate::plan::{mutable::projections::OwnedProjection, ProjectionPlan};

impl Copying<'_> {
    pub(crate) fn take_projection(
        &mut self,
        projection: OwnedProjection,
    ) -> Result<ProjectionPlan> {
        Ok(ProjectionPlan {
            expr: self.take_scalar(projection.expression)?,
            alias: self.field("alias", |copying| {
                projection
                    .alias
                    .map(|alias| {
                        let (alias, memory) = alias.into_parts();
                        let alias = copying.control.finish(alias, memory)?;
                        Ok(copying.take(alias))
                    })
                    .transpose()
            })?,
        })
    }
}
