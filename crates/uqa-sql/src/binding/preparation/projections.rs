//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bind SELECT output expressions and wildcard fields in their written order.

use super::ownership::AnalysisIdentity;
use super::{
    ColumnType, ExpressionType, LiteralInput, Preparation, QueryOutput, QueryPlan, RowSchema,
    SQLError, ScalarExpr, SchemaContext,
};
use crate::catalog::collations::derivation::Derivation;
use crate::catalog::work::{inline, ReferenceWork, Work};
use crate::plan::ProjectionPlan;
use crate::semantics::ProjectionStarMetadata;
use uqa_core::memory::ProductionControl;

impl<I: LiteralInput, C: SchemaContext> Preparation<'_, I, C> {
    pub(super) fn projections<'a>(
        &mut self,
        projections: impl IntoIterator<Item = &'a ProjectionPlan>,
        source: &RowSchema,
        input: &RowSchema,
        subqueries: &[QueryPlan],
    ) -> Result<QueryOutput, SQLError> {
        inline(self.projections_with_work(projections, source, input, subqueries, &mut ()))
    }

    pub(super) async fn projections_with_work<'a, W: Work + Send>(
        &mut self,
        projections: impl IntoIterator<Item = &'a ProjectionPlan>,
        source: &RowSchema,
        input: &RowSchema,
        subqueries: &[QueryPlan],
        work: &mut W,
    ) -> Result<QueryOutput, SQLError> {
        let owner = self.scope.control().capture();
        let control = owner.control();
        let mut input_work = ReferenceWork {
            control: &control,
            work,
        };
        let mut output = QueryOutput::new(&control);
        for projection in projections {
            input_work.checkpoint().await?;
            let expansion = if matches!(projection.expr, ScalarExpr::QualifiedStar(_)) {
                input
            } else {
                source
            };
            if super::super::projection::visit_projection_star_metadata_with_work(
                &projection.expr,
                expansion,
                &control,
                input_work.work,
                &mut |field| output.push_star_field(field, &control),
            )
            .await?
            {
                output.open |= match &projection.expr {
                    ScalarExpr::QualifiedStar(qualifier) => {
                        expansion.columns_are_open(Some(qualifier))
                    }
                    _ => expansion.columns_are_open(None),
                };
            } else {
                let mut value = self
                    .expression_with_work(&projection.expr, input, subqueries, input_work.work)
                    .await?;
                if matches!(value.ty.as_deref(), Some(ColumnType::Composite(_))) {
                    value.record_fields = self.scope.bind_record_fields(
                        self.routines,
                        &projection.expr,
                        input,
                        subqueries,
                        &self.parameters.values()?,
                    )?;
                }
                if matches!(value.derivation, Derivation::Conflict(..)) {
                    output.key_origins.push(
                        (output.types.len(), AnalysisIdentity::from(&projection.expr)),
                        &control,
                    )?;
                    self.parameters
                        .collations
                        .record_target(&projection.expr, value.derivation)?;
                }
                let site_label = projection
                    .alias
                    .is_none()
                    .then(|| {
                        self.scope.variable_sites.as_ref().and_then(|sites| {
                            sites
                                .site(&projection.expr)
                                .and_then(|site| sites.label(site))
                        })
                    })
                    .flatten();
                let name = if let Some(name) = site_label {
                    control.copy_text(name)?
                } else {
                    crate::semantics::with_projection_label(projection, &control, |name| {
                        Ok(control.copy_text(name)?)
                    })?
                };
                output.columns.push(name, &control)?;
                output.types.push(value, &control)?;
            }
        }
        Ok(output)
    }

    pub(super) fn resolve_targets(&mut self, output: &mut QueryOutput) -> Result<(), SQLError> {
        for ty in &mut output.types {
            self.parameters.coerce_unknown(ty, &ColumnType::Text)?;
        }
        Ok(())
    }
}

impl QueryOutput {
    fn push_star_field(
        &mut self,
        field: ProjectionStarMetadata<'_>,
        control: &ProductionControl<'_>,
    ) -> Result<(), SQLError> {
        self.columns.push(control.copy_text(field.name)?, control)?;
        let field_type = field
            .ty
            .map(|ty| ty.clone_with_control(control))
            .transpose()?;
        let mut value = ExpressionType::resolved_produced(field_type, control)?;
        if let Some(oid) = field.collation {
            value.derivation = Derivation::implicit(i64::from(oid));
        }
        if let Some(record) = field.record {
            value.record_fields.clone_from(&record.types);
            value.record_descriptor.clone_from(&record.descriptor);
        }
        self.types.push(value, control)?;
        Ok(())
    }
}
