//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Projection containers move each scalar and alias with its exact existing constructor owner.

use super::{Copying, MutableQueryPlan, OwnedScalarExpr, RelationalPlan, RewriteMemory};
use crate::{plan::ProjectionPlan, RowSchema, SQLError};
use uqa_core::memory::{Budgeted, Produced, ProductionControl};

pub struct OwnedProjection {
    pub expression: OwnedScalarExpr,
    pub alias: Option<Produced<String>>,
}

pub struct OwnedProjectionIter {
    projections: std::vec::IntoIter<ProjectionPlan>,
    memory: Option<RewriteMemory>,
    index: usize,
}

impl Iterator for OwnedProjectionIter {
    type Item = OwnedProjection;

    fn next(&mut self) -> Option<OwnedProjection> {
        let projection = self.projections.next()?;
        let (scalar, alias) = if let Some(memory) = &mut self.memory {
            let mut item = memory
                .item_mut(self.index)
                .take()
                .expect("projection owner");
            let scalar = match item.scalar_mut().take().expect("scalar owner") {
                RewriteMemory::Scalar(memory) => Some(memory),
                RewriteMemory::Fields { .. } => unreachable!("projection scalar owner"),
            };
            let alias = item
                .field_mut("alias")
                .take()
                .map(RewriteMemory::into_reservation);
            (scalar, alias)
        } else {
            (None, None)
        };
        self.index += 1;
        Some(OwnedProjection {
            expression: OwnedScalarExpr::from_rewrite_parts(projection.expr, scalar),
            alias: projection.alias.map(|value| match alias {
                Some(memory) => Produced::from(Budgeted::new(value, memory)),
                None => ProductionControl::uncontrolled()
                    .finish(value, None)
                    .expect("ordinary projection alias"),
            }),
        })
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        self.projections.size_hint()
    }
}

impl ExactSizeIterator for OwnedProjectionIter {}

struct ProjectionVector {
    projections: Vec<ProjectionPlan>,
    memory: Option<RewriteMemory>,
}

impl ProjectionVector {
    fn new(
        projections: impl ExactSizeIterator<Item = OwnedProjection>,
        control: &ProductionControl<'_>,
    ) -> Result<Self, SQLError> {
        control.check()?;
        let Some(mut copying) = Copying::for_rewrite(*control) else {
            let projections = projections
                .map(|projection| {
                    Ok(ProjectionPlan {
                        expr: projection
                            .expression
                            .finish(control)?
                            .into_uncontrolled()
                            .expect("ordinary projection"),
                        alias: projection
                            .alias
                            .map(|alias| alias.into_uncontrolled().expect("ordinary alias")),
                    })
                })
                .collect::<Result<_, SQLError>>()?;
            return Ok(Self {
                projections,
                memory: None,
            });
        };
        let projections = copying.map(projections, Copying::take_projection)?;
        Ok(Self {
            projections,
            memory: Some(copying.into_rewrite_memory()),
        })
    }
}

impl MutableQueryPlan {
    /// Edit the projection list and its scalar-query namespace together, moving existing projection children without copying their payloads.
    pub fn map_projection_inputs<I: ExactSizeIterator<Item = OwnedProjection>>(
        self,
        control: &ProductionControl<'_>,
        map: impl FnOnce(OwnedProjectionIter, &mut super::ScalarQueryArena) -> Result<I, SQLError>,
    ) -> Result<Self, SQLError> {
        let (mut query, projections) = self.take_projections(control)?;
        let mut arena = query.take_arena();
        let projections = map(projections, &mut arena)?;
        query = query.replace_projections(projections, control)?;
        query.restore_arena(arena);
        Ok(query)
    }

    /// Move all projections into an owning iterator. The remaining query has an empty projection vector; its other constructors are unchanged.
    pub fn take_projections(
        mut self,
        control: &ProductionControl<'_>,
    ) -> Result<(Self, OwnedProjectionIter), SQLError> {
        self.assert_owner(control);
        control.check()?;
        let RelationalPlan::QueryBlock(block) = &mut self.query.root else {
            return Err(SQLError::Internal(
                "projection extraction requires a query block".into(),
            ));
        };
        let projections = std::mem::take(&mut block.projections).into_iter();
        let memory = self
            .memory
            .as_mut()
            .and_then(|memory| projection_memory(memory).take());
        Ok((
            self,
            OwnedProjectionIter {
                projections,
                memory,
                index: 0,
            },
        ))
    }

    /// Admit the destination vector and field records, then transfer each existing scalar and alias. Removed projections are freed before their old reservations.
    pub fn replace_projections(
        mut self,
        projections: impl ExactSizeIterator<Item = OwnedProjection>,
        control: &ProductionControl<'_>,
    ) -> Result<Self, SQLError> {
        self.assert_owner(control);
        control.check()?;
        let replacement = ProjectionVector::new(projections, control)?;
        let RelationalPlan::QueryBlock(block) = &mut self.query.root else {
            return Err(SQLError::Internal(
                "projection replacement requires a query block".into(),
            ));
        };
        block.projections = replacement.projections;
        if let Some(memory) = &mut self.memory {
            *projection_memory(memory) = replacement.memory;
        }
        control.check()?;
        Ok(self)
    }

    /// Retain an already published immutable output descriptor without copying its columns or charging its shared owner again.
    pub fn set_analyzed_output(
        mut self,
        schema: Option<RowSchema>,
        control: &ProductionControl<'_>,
    ) -> Result<Self, SQLError> {
        self.assert_owner(control);
        control.check()?;
        self.query.analyzed_output = schema;
        Ok(self)
    }
}

fn projection_memory(memory: &mut RewriteMemory) -> &mut Option<RewriteMemory> {
    memory
        .field_mut("root")
        .as_mut()
        .expect("query root")
        .field_mut("block")
        .as_mut()
        .expect("query block")
        .field_mut("projections")
}
