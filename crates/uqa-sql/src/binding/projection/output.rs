//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Output columns retain each selected type until one admitted row schema is published.

use super::{ProjectionMetadata, RowSchema, SQLError, ScalarExpr};
use uqa_core::memory::{Produced, ProductionControl, ProductionVec};

struct Column {
    name: Produced<String>,
    metadata: ProjectionMetadata,
}

pub(in crate::binding) struct ProjectionOutput<'a> {
    columns: ProductionVec<'a, Column>,
    control: ProductionControl<'a>,
}

impl<'a> ProjectionOutput<'a> {
    pub(in crate::binding) fn new(control: ProductionControl<'a>) -> Self {
        Self {
            columns: ProductionVec::new(control),
            control,
        }
    }

    pub(in crate::binding) fn len(&self) -> usize {
        self.columns.len()
    }

    pub(in crate::binding) fn push(
        &mut self,
        name: &str,
        metadata: ProjectionMetadata,
    ) -> Result<(), SQLError> {
        let column = Column {
            name: self.control.copy_text(name)?,
            metadata,
        };
        self.columns.push_produced(
            self.control
                .finish(column, self.control.empty_reservation())?,
        )?;
        Ok(())
    }

    pub(in crate::binding) fn star(
        &mut self,
        expression: &ScalarExpr,
        schema: &RowSchema,
    ) -> Result<bool, SQLError> {
        let control = self.control;
        super::visit_projection_star_metadata(expression, schema, &control, &mut |field| {
            let ty = field
                .ty
                .map(|ty| ty.clone_with_control(&control))
                .transpose()?;
            self.push(
                field.name,
                ProjectionMetadata {
                    ty,
                    collation: field.collation,
                    record: field.record.cloned(),
                },
            )
        })
    }

    pub(in crate::binding) fn finish(self) -> Result<RowSchema, SQLError> {
        RowSchema::with_output_metadata_with_control(
            None,
            self.columns.iter().map(|column| {
                (
                    column.name.as_str(),
                    column.metadata.ty.as_deref(),
                    column.metadata.collation,
                    column.metadata.record.clone(),
                )
            }),
            false,
            &self.control,
        )
    }
}
