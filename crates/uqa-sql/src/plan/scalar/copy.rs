//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Admission-aware copies of already analyzed scalar expressions. Query children remain references into their enclosing plan, as in an ordinary scalar clone.

use super::resources::input::Work;
use super::{
    owned::{OwnedScalarExpr, ScalarMemory},
    resources::{Control, Lowering, Result},
    ScalarExpr,
};
use uqa_core::memory::{Produced, ProductionControl};

mod iterative;

impl ScalarExpr {
    /// Copy a scalar tree using admitted constructor frames instead of recursive scalar calls. The immutable result combines only destination reservations; temporary frame storage is released before return.
    pub fn clone_with_control(&self, control: &ProductionControl<'_>) -> Result<Produced<Self>> {
        iterative::immutable(self, *control)
    }

    /// Capture scalar syntax and evaluated literals under independent ownership; query-arena references keep their original positions.
    pub(crate) fn capture_with_footprint(
        &self,
        control: &ProductionControl<'_>,
    ) -> Result<(Produced<Self>, usize)> {
        iterative::capture(self, *control)
    }

    pub(in crate::plan) async fn capture_with_footprint_from<W: Work>(
        &self,
        control: &ProductionControl<'_>,
        work: &mut W,
    ) -> std::result::Result<(Produced<Self>, usize), W::Error>
    where
        W::Error: From<crate::schema::retention::CatalogRetentionError>,
    {
        iterative::capture_from(self, *control, work).await
    }

    /// Copy into a mutable scalar owner with independent direct and child reservations. Iterative constructors preserve field order, destination admission and query-arena identities in both ordinary and controlled modes.
    pub fn copy_for_rewrite(&self, control: &ProductionControl<'_>) -> Result<OwnedScalarExpr> {
        iterative::mutable(self, *control)
    }

    pub(crate) fn copy_for_rewrite_from<'a, W: Work>(
        &'a self,
        control: &ProductionControl<'a>,
        work: &'a mut W,
    ) -> impl std::future::Future<Output = std::result::Result<OwnedScalarExpr, W::Error>> + 'a
    where
        W::Error: From<crate::schema::retention::CatalogRetentionError>,
    {
        iterative::mutable_from(self, *control, work)
    }
}

impl Lowering<'_> {
    pub(super) fn scalar_type_copy(
        &mut self,
        ty: Option<&crate::ast::ColumnType>,
    ) -> Result<Option<crate::ast::ColumnType>> {
        crate::catalog::work::inline(self.type_from(ty, &mut ()))
    }

    async fn scalar_leaf_from<W: Work>(
        &mut self,
        expression: &ScalarExpr,
        work: &mut W,
    ) -> std::result::Result<ScalarExpr, W::Error> {
        self.checkpoint(work).await?;
        Ok(match expression {
            ScalarExpr::Star => ScalarExpr::Star,
            ScalarExpr::QualifiedStar(name) => {
                ScalarExpr::QualifiedStar(self.reference_from(name, work).await?)
            }
            ScalarExpr::Default => ScalarExpr::Default,
            ScalarExpr::Column(name) => ScalarExpr::Column(self.reference_from(name, work).await?),
            ScalarExpr::Position(position) => ScalarExpr::Position(*position),
            ScalarExpr::InternalColumn(column) => ScalarExpr::InternalColumn(*column),
            ScalarExpr::QualifiedColumn { qualifier, column } => ScalarExpr::QualifiedColumn {
                qualifier: self.text_from(qualifier, work).await?,
                column: self.reference_from(column, work).await?,
            },
            ScalarExpr::Literal(value) => ScalarExpr::Literal(self.value_from(value, work).await?),
            ScalarExpr::RetainedLiteral(literal) => ScalarExpr::RetainedLiteral(literal.clone()),
            ScalarExpr::TypedLiteral {
                value,
                ty,
                bound_type,
                parameter_index,
                collation_oid,
                composite_source,
            } => {
                let bound_type = self.type_from(bound_type.as_ref(), work).await?;
                ScalarExpr::TypedLiteral {
                    composite_source: self
                        .composite_source_from(composite_source.as_deref(), work)
                        .await?,
                    value: self.value_from(value, work).await?,
                    ty: self.text_from(ty, work).await?,
                    bound_type,
                    parameter_index: *parameter_index,
                    collation_oid: *collation_oid,
                }
            }
            ScalarExpr::Param(index) => ScalarExpr::Param(*index),
            ScalarExpr::ScalarSubquery(query) => ScalarExpr::ScalarSubquery(*query),
            ScalarExpr::Exists { subquery, negated } => ScalarExpr::Exists {
                subquery: *subquery,
                negated: *negated,
            },
            ScalarExpr::Func { .. }
            | ScalarExpr::Array(_)
            | ScalarExpr::CompositeRow { .. }
            | ScalarExpr::Row(_)
            | ScalarExpr::Binary { .. }
            | ScalarExpr::UnaryMinus(_)
            | ScalarExpr::Not(_)
            | ScalarExpr::And(_)
            | ScalarExpr::Or(_)
            | ScalarExpr::IsNull { .. }
            | ScalarExpr::Between { .. }
            | ScalarExpr::InList { .. }
            | ScalarExpr::WindowCall { .. }
            | ScalarExpr::Case { .. }
            | ScalarExpr::Collated { .. }
            | ScalarExpr::Cast { .. }
            | ScalarExpr::InSubquery { .. } => unreachable!("scalar leaf constructor"),
        })
    }
}

#[cfg(test)]
mod tests;
