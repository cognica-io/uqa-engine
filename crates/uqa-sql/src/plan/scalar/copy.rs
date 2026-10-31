//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Admission-aware copies of already analyzed scalar expressions. Query children remain references into their enclosing plan, as in an ordinary scalar clone.

use super::{
    owned::{OwnedScalarExpr, ScalarMemory},
    resources::{Control, Lowering, Result},
    source::Source,
    ScalarExpr,
};
use uqa_core::memory::{Produced, ProductionControl};

mod iterative;

impl ScalarExpr {
    /// Copy a scalar tree using admitted constructor frames instead of recursive scalar calls. The immutable result combines only destination reservations; temporary frame storage is released before return.
    pub fn clone_with_control(&self, control: &ProductionControl<'_>) -> Result<Produced<Self>> {
        iterative::immutable(self, *control)
    }

    /// Copy into a mutable scalar owner with independent direct and child reservations. Iterative constructors preserve field order, destination admission and query-arena identities in both ordinary and controlled modes.
    pub fn copy_for_rewrite(&self, control: &ProductionControl<'_>) -> Result<OwnedScalarExpr> {
        iterative::mutable(self, *control)
    }
}

impl Lowering<'_> {
    pub(super) fn scalar_type_copy(
        &mut self,
        ty: Option<&crate::ast::ColumnType>,
    ) -> Result<Option<crate::ast::ColumnType>> {
        ty.map(|ty| {
            let Some(control) = self.control.as_mut() else {
                return Ok(ty.clone());
            };
            let (ty, memory) = ty.clone_with_control(&control.production)?.into_parts();
            control.memory.absorb(memory.expect("controlled type copy"));
            Ok(ty)
        })
        .transpose()
    }

    fn scalar_leaf_copy(&mut self, expression: &ScalarExpr) -> Result<ScalarExpr> {
        self.check()?;
        Ok(match expression {
            ScalarExpr::Star => ScalarExpr::Star,
            ScalarExpr::QualifiedStar(name) => {
                ScalarExpr::QualifiedStar(self.copy_reference_name(name)?)
            }
            ScalarExpr::Default => ScalarExpr::Default,
            ScalarExpr::Column(name) => ScalarExpr::Column(self.copy_reference_name(name)?),
            ScalarExpr::Position(position) => ScalarExpr::Position(*position),
            ScalarExpr::InternalColumn(column) => ScalarExpr::InternalColumn(*column),
            ScalarExpr::QualifiedColumn { qualifier, column } => ScalarExpr::QualifiedColumn {
                qualifier: self.copy_text(qualifier)?,
                column: self.copy_reference_name(column)?,
            },
            ScalarExpr::Literal(value) => ScalarExpr::Literal(self.value(Source::Borrowed(value))?),
            ScalarExpr::RetainedLiteral(literal) => ScalarExpr::RetainedLiteral(literal.clone()),
            ScalarExpr::TypedLiteral {
                value,
                ty,
                bound_type,
                parameter_index,
                collation_oid,
                composite_source,
            } => {
                let bound_type = self.scalar_type_copy(bound_type.as_ref())?;
                ScalarExpr::TypedLiteral {
                    composite_source: self
                        .composite_source(composite_source.as_ref().map(Source::Borrowed))?,
                    value: self.value(Source::Borrowed(value))?,
                    ty: self.copy_text(ty)?,
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
