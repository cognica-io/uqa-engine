//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Physical scalar construction consumes admitted expressions and releases each discarded constructor before publishing its schema.

use super::{Project, ProjectionTarget};
use crate::{
    batch::ProjectedSlot, PhysicalOperator, RowSchema, ScalarExpr, SharedExpressionEvaluator,
};
use uqa_core::memory::{Produced, ProductionControl, ProductionVec};
use uqa_sql::{plan::OwnedScalarExpr, ColumnType, SQLError};

struct Column {
    name: Produced<String>,
    ty: Option<Produced<ColumnType>>,
    source: ProjectedSlot,
}
struct Internal {
    column: uqa_sql::ast::InternalColumnRef,
    ty: Option<Produced<ColumnType>>,
    source: ProjectedSlot,
}

impl<'a> Project<'a> {
    /// Consume independently owned scalar and target fields. No rewritten or discarded input remains charged after construction.
    pub fn with_owned_target_evaluator(
        child: Box<dyn PhysicalOperator + 'a>,
        projections: impl IntoIterator<
            Item = Result<(Produced<ProjectionTarget>, OwnedScalarExpr), SQLError>,
        >,
        evaluator: SharedExpressionEvaluator<'a>,
        control: &ProductionControl<'_>,
    ) -> Result<Self, SQLError> {
        let (child, (schema, computed)) =
            crate::physical::construction::OperatorBuild::run(child, |child| {
                let inputs = ProjectionInputs::collect(projections, control)?;
                let inputs = inputs.bind(child.row_schema(), &evaluator, control)?;
                let mut layout = Layout::new(child.row_schema(), &evaluator, control);
                for projection in inputs {
                    control.check()?;
                    layout.append(projection)?;
                }
                layout.finish()
            })?;
        Ok(Self {
            child,
            input_pending: crate::physical::demand::PendingRows::default(),
            computed,
            evaluator,
            schema,
            ordering: Vec::new(),
            work_mem: None,
            pending: None,
        })
    }
}

type OwnedProjection = (Produced<ProjectionTarget>, OwnedScalarExpr);

/// Keep the admitted input array alive until its consuming traversal frees the last slot. Each expression and target retains an independent field owner.
struct ProjectionInputs {
    values: std::vec::IntoIter<OwnedProjection>,
    _memory: Option<uqa_core::memory::MemoryReservation>,
}

impl ProjectionInputs {
    fn from_produced(input: Produced<Vec<OwnedProjection>>) -> Self {
        let (values, memory) = input.into_parts();
        Self {
            values: values.into_iter(),
            _memory: memory,
        }
    }

    fn collect(
        projections: impl IntoIterator<Item = Result<OwnedProjection, SQLError>>,
        control: &ProductionControl<'_>,
    ) -> Result<Self, SQLError> {
        let mut inputs = ProductionVec::new(*control);
        for projection in projections {
            control.check()?;
            inputs.push_produced(control.finish(projection?, control.empty_reservation())?)?;
        }
        Ok(Self::from_produced(inputs.finish()?))
    }

    fn bind(
        self,
        schema: &RowSchema,
        evaluator: &SharedExpressionEvaluator<'_>,
        control: &ProductionControl<'_>,
    ) -> Result<Self, SQLError> {
        let mut bound = ProductionVec::new(*control);
        for (target, expression) in self {
            let expression =
                evaluator.bind_type_introspection_owned(expression, schema, control)?;
            bound.push_produced(
                control.finish((target, expression), control.empty_reservation())?,
            )?;
        }
        Ok(Self::from_produced(bound.finish()?))
    }
}

impl Iterator for ProjectionInputs {
    type Item = OwnedProjection;
    fn next(&mut self) -> Option<Self::Item> {
        self.values.next()
    }
    fn size_hint(&self) -> (usize, Option<usize>) {
        self.values.size_hint()
    }
}
impl ExactSizeIterator for ProjectionInputs {}

type Computed = crate::scalar::PreparedExpressions<Produced<Vec<ScalarExpr>>>;

struct Layout<'a, 'e> {
    input: &'a RowSchema,
    evaluator: &'a SharedExpressionEvaluator<'e>,
    control: ProductionControl<'a>,
    projected: ProductionVec<'a, Column>,
    internal: ProductionVec<'a, Internal>,
    computed: ProductionVec<'a, ScalarExpr>,
}

impl<'a, 'e> Layout<'a, 'e> {
    fn new(
        input: &'a RowSchema,
        evaluator: &'a SharedExpressionEvaluator<'e>,
        control: &ProductionControl<'a>,
    ) -> Self {
        Self {
            input,
            evaluator,
            control: *control,
            projected: ProductionVec::new(*control),
            internal: ProductionVec::new(*control),
            computed: ProductionVec::new(*control),
        }
    }
    fn append(
        &mut self,
        (target, expression): (Produced<ProjectionTarget>, OwnedScalarExpr),
    ) -> Result<(), SQLError> {
        let input = self.input;
        let evaluator = self.evaluator;
        let control = &self.control;
        if uqa_sql::semantics::visit_bound_projection_star_metadata(
            &expression,
            input,
            input,
            control,
            &mut |field| {
                if field
                    .logical
                    .is_some_and(|position| !evaluator.star_position_visible(input, position))
                {
                    return Ok(());
                }
                let physical = field
                    .logical
                    .and_then(|position| input.physical_slot(position))
                    .or_else(|| {
                        field
                            .qualifier
                            .and_then(|qualifier| input.qualified_slot(qualifier, field.name))
                    });
                let column = Column {
                    name: control.copy_text(field.name)?,
                    ty: field
                        .ty
                        .map(|ty| ty.clone_with_control(control))
                        .transpose()?,
                    source: ProjectedSlot::BoundInput {
                        physical,
                        collation: field.collation,
                    },
                };
                self.projected
                    .push_produced(control.finish(column, control.empty_reservation())?)?;
                Ok(())
            },
        )? {
            assert!(
                matches!(&*target, ProjectionTarget::Column(_)),
                "internal projection cannot expand a wildcard"
            );
            return Ok(());
        }
        let ty = match evaluator.expression_type_with_control(&expression, input, control) {
            Ok(ty) => ty,
            Err(error) if matches!(error.sqlstate(), Some("53200" | "57014")) => return Err(error),
            Err(_) => None,
        };
        let source = if let Some(source) = input.projection_input(&expression) {
            source
        } else {
            let source = ProjectedSlot::Computed {
                position: self.computed.len(),
                collation: input.input_collation(&expression),
            };
            self.computed.push_produced(expression.finish(control)?)?;
            source
        };
        let (target, memory) = target.into_parts();
        match target {
            ProjectionTarget::Column(name) => self.projected.push_produced(control.finish(
                Column {
                    name: control.finish(name, memory)?,
                    ty,
                    source,
                },
                control.empty_reservation(),
            )?)?,
            ProjectionTarget::Internal(column) => {
                let _target = control.finish(column, memory)?;
                self.internal.push_produced(
                    control.finish(Internal { column, ty, source }, control.empty_reservation())?,
                )?;
            }
        }
        Ok(())
    }
    fn finish(self) -> Result<(RowSchema, Computed), SQLError> {
        let input = self.input;
        let control = &self.control;
        let mut internal_fields = ProductionVec::new(*control);
        for field in self.internal.iter() {
            internal_fields.push_copy((field.column, field.ty.as_deref(), field.source))?;
        }
        let schema = RowSchema::project_with_sources_with_control(
            input,
            self.projected
                .iter()
                .map(|column| (column.name.as_str(), column.ty.as_deref(), column.source)),
            &internal_fields,
            self.computed.len(),
            false,
            control,
        )?;
        let computed =
            crate::scalar::PreparedExpressions::produced_scalars(self.computed.finish()?, control)?;
        Ok((schema, computed))
    }
}

#[cfg(test)]
mod tests;
