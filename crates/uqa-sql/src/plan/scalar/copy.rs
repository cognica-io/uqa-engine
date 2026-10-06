//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Admission-aware copies of already analyzed scalar expressions. Query children remain references into their enclosing plan, as in an ordinary scalar clone.

use super::{
    resources::{Control, Lowering, Result},
    source::Source,
    ScalarExpr, ScalarFrameBound, ScalarOrder, ScalarWindowFrame, ScalarWindowSpec,
};
use uqa_core::memory::{Produced, ProductionControl};

impl ScalarExpr {
    /// Copy a scalar tree, admitting every destination allocation before creating it and retaining the complete reservation. Subquery identities still refer to the enclosing plan's query arena.
    pub fn clone_with_control(&self, control: &ProductionControl<'_>) -> Result<Produced<Self>> {
        control.check()?;
        if control.budget().is_none() {
            return Ok(control.finish(self.clone(), None)?);
        }
        let mut copying = Lowering {
            control: Some(Control::from_production(*control)),
        };
        let expression = copying.scalar_copy(self)?;
        Ok(copying.finish(expression)?.into())
    }
}

impl Lowering<'_> {
    #[expect(
        clippy::too_many_lines,
        reason = "exhaustive scalar copying shares the lowerer's destination admission"
    )]
    fn scalar_copy(&mut self, expression: &ScalarExpr) -> Result<ScalarExpr> {
        self.check()?;
        Ok(match expression {
            ScalarExpr::Star => ScalarExpr::Star,
            ScalarExpr::QualifiedStar(name) => ScalarExpr::QualifiedStar(self.copy_text(name)?),
            ScalarExpr::Default => ScalarExpr::Default,
            ScalarExpr::Column(name) => ScalarExpr::Column(self.copy_text(name)?),
            ScalarExpr::Position(position) => ScalarExpr::Position(*position),
            ScalarExpr::InternalColumn(column) => ScalarExpr::InternalColumn(*column),
            ScalarExpr::QualifiedColumn { qualifier, column } => ScalarExpr::QualifiedColumn {
                qualifier: self.copy_text(qualifier)?,
                column: self.copy_text(column)?,
            },
            ScalarExpr::Literal(value) => ScalarExpr::Literal(self.value(Source::Borrowed(value))?),
            ScalarExpr::TypedLiteral {
                value,
                ty,
                bound_type,
                parameter_index,
            } => {
                let bound_type = bound_type
                    .as_ref()
                    .map(|ty| {
                        let control = self.control.as_mut().expect("controlled scalar copy");
                        let (ty, memory) = ty.clone_with_control(&control.production)?.into_parts();
                        control.memory.absorb(memory.expect("controlled type copy"));
                        Ok::<_, crate::schema::retention::CatalogRetentionError>(ty)
                    })
                    .transpose()?;
                ScalarExpr::TypedLiteral {
                    value: self.value(Source::Borrowed(value))?,
                    ty: self.copy_text(ty)?,
                    bound_type,
                    parameter_index: *parameter_index,
                }
            }
            ScalarExpr::Param(index) => ScalarExpr::Param(*index),
            ScalarExpr::Func {
                name,
                binding,
                args,
                distinct,
                order_by,
                order_syntax,
                filter,
            } => ScalarExpr::Func {
                name: self.copy_text(name)?,
                binding: binding
                    .as_ref()
                    .map(|binding| self.binding(Source::Borrowed(binding)))
                    .transpose()?,
                args: self.map(args.iter(), Self::scalar_copy)?,
                distinct: *distinct,
                order_by: self.map(order_by.iter(), Self::scalar_order_copy)?,
                order_syntax: *order_syntax,
                filter: self.scalar_optional_copy(filter.as_deref())?,
            },
            ScalarExpr::Array(items) => {
                ScalarExpr::Array(self.map(items.iter(), Self::scalar_copy)?)
            }
            ScalarExpr::Row(items) => ScalarExpr::Row(self.map(items.iter(), Self::scalar_copy)?),
            ScalarExpr::Binary { op, lhs, rhs } => ScalarExpr::Binary {
                op: *op,
                lhs: self.scalar_box_copy(lhs)?,
                rhs: self.scalar_box_copy(rhs)?,
            },
            ScalarExpr::UnaryMinus(expression) => {
                ScalarExpr::UnaryMinus(self.scalar_box_copy(expression)?)
            }
            ScalarExpr::Not(expression) => ScalarExpr::Not(self.scalar_box_copy(expression)?),
            ScalarExpr::And(items) => ScalarExpr::And(self.map(items.iter(), Self::scalar_copy)?),
            ScalarExpr::Or(items) => ScalarExpr::Or(self.map(items.iter(), Self::scalar_copy)?),
            ScalarExpr::IsNull { expr, negated } => ScalarExpr::IsNull {
                expr: self.scalar_box_copy(expr)?,
                negated: *negated,
            },
            ScalarExpr::Between { expr, low, high } => ScalarExpr::Between {
                expr: self.scalar_box_copy(expr)?,
                low: self.scalar_box_copy(low)?,
                high: self.scalar_box_copy(high)?,
            },
            ScalarExpr::InList {
                expr,
                list,
                negated,
            } => ScalarExpr::InList {
                expr: self.scalar_box_copy(expr)?,
                list: self.map(list.iter(), Self::scalar_copy)?,
                negated: *negated,
            },
            ScalarExpr::WindowCall {
                name,
                args,
                spec,
                filter,
                modifiers,
            } => ScalarExpr::WindowCall {
                name: self.copy_text(name)?,
                args: self.map(args.iter(), Self::scalar_copy)?,
                spec: ScalarWindowSpec {
                    definition: spec.definition,
                    partition_by: self.map(spec.partition_by.iter(), Self::scalar_copy)?,
                    order_by: self.map(spec.order_by.iter(), Self::scalar_order_copy)?,
                    frame: spec
                        .frame
                        .as_ref()
                        .map(|frame| {
                            Ok::<_, crate::schema::retention::CatalogRetentionError>(
                                ScalarWindowFrame {
                                    mode: frame.mode,
                                    start: self.scalar_bound_copy(&frame.start)?,
                                    end: self.scalar_bound_copy(&frame.end)?,
                                    between: frame.between,
                                    exclusion: frame.exclusion,
                                },
                            )
                        })
                        .transpose()?,
                },
                filter: self.scalar_optional_copy(filter.as_deref())?,
                modifiers: *modifiers,
            },
            ScalarExpr::Case {
                base,
                when,
                else_branch,
            } => ScalarExpr::Case {
                base: self.scalar_optional_copy(base.as_deref())?,
                when: self.map(when.iter(), |this, (condition, value)| {
                    Ok((this.scalar_copy(condition)?, this.scalar_copy(value)?))
                })?,
                else_branch: self.scalar_optional_copy(else_branch.as_deref())?,
            },
            ScalarExpr::Cast { implicit, expr, ty } => ScalarExpr::Cast {
                implicit: *implicit,
                expr: self.scalar_box_copy(expr)?,
                ty: self.copy_text(ty)?,
            },
            ScalarExpr::ScalarSubquery(query) => ScalarExpr::ScalarSubquery(*query),
            ScalarExpr::Exists { subquery, negated } => ScalarExpr::Exists {
                subquery: *subquery,
                negated: *negated,
            },
            ScalarExpr::InSubquery {
                expr,
                subquery,
                negated,
            } => ScalarExpr::InSubquery {
                expr: self.scalar_box_copy(expr)?,
                subquery: *subquery,
                negated: *negated,
            },
        })
    }

    fn scalar_box_copy(&mut self, expression: &ScalarExpr) -> Result<Box<ScalarExpr>> {
        self.boxed(|this| this.scalar_copy(expression))
    }

    fn scalar_optional_copy(
        &mut self,
        expression: Option<&ScalarExpr>,
    ) -> Result<Option<Box<ScalarExpr>>> {
        expression
            .map(|expression| self.scalar_box_copy(expression))
            .transpose()
    }

    fn scalar_order_copy(&mut self, order: &ScalarOrder) -> Result<ScalarOrder> {
        Ok(ScalarOrder {
            expr: self.scalar_copy(&order.expr)?,
            descending: order.descending,
            nulls: order.nulls,
        })
    }

    fn scalar_bound_copy(&mut self, bound: &ScalarFrameBound) -> Result<ScalarFrameBound> {
        Ok(match bound {
            ScalarFrameBound::UnboundedPreceding => ScalarFrameBound::UnboundedPreceding,
            ScalarFrameBound::UnboundedFollowing => ScalarFrameBound::UnboundedFollowing,
            ScalarFrameBound::CurrentRow => ScalarFrameBound::CurrentRow,
            ScalarFrameBound::Preceding(expression) => {
                ScalarFrameBound::Preceding(self.scalar_box_copy(expression)?)
            }
            ScalarFrameBound::Following(expression) => {
                ScalarFrameBound::Following(self.scalar_box_copy(expression)?)
            }
        })
    }
}

#[cfg(test)]
mod tests;
