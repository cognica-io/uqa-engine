//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{Expr, Lowering, ScalarExpr, ScalarQueryInput};
use crate::plan::scalar::resources::input::{boxed_from, map_from};
use crate::plan::scalar::ScalarInputWork;
use crate::{
    ast::{FrameBound, OrderBy, WindowSpec},
    ir::{ScalarFrameBound, ScalarOrder, ScalarWindowFrame, ScalarWindowSpec},
};

impl Lowering<'_> {
    #[expect(
        clippy::too_many_lines,
        reason = "the exhaustive AST constructor retains every scalar field"
    )]
    pub(super) async fn scalar_node_from<
        's,
        W: ScalarInputWork,
        Q: ScalarQueryInput<&'s crate::ast::Expr, &'s crate::ast::SelectStmt, W::Error>,
    >(
        &mut self,
        source: &'s Expr,
        queries: &mut Q,
        work: &mut W,
    ) -> Result<ScalarExpr, W::Error> {
        self.checkpoint(work).await?;
        Ok(match source {
            Expr::Star => ScalarExpr::Star,
            Expr::QualifiedStar(name) => {
                ScalarExpr::QualifiedStar(self.reference_from(name, work).await?)
            }
            Expr::Default => ScalarExpr::Default,
            Expr::Column(name) => ScalarExpr::Column(self.reference_from(name, work).await?),
            Expr::QualifiedColumn { qualifier, column } => ScalarExpr::QualifiedColumn {
                qualifier: self.text_from(qualifier, work).await?,
                column: self.reference_from(column, work).await?,
            },
            Expr::InternalColumn(column) => ScalarExpr::InternalColumn(*column),
            Expr::Literal(value) => ScalarExpr::Literal(self.value_from(value, work).await?),
            Expr::RetainedLiteral(value) => ScalarExpr::RetainedLiteral(value.0.clone()),
            Expr::TypedLiteral {
                value,
                ty,
                collation_oid,
                composite_source,
            } => ScalarExpr::TypedLiteral {
                collation_oid: *collation_oid,
                composite_source: self
                    .composite_source_from(composite_source.as_deref(), work)
                    .await?,
                value: self.value_from(value, work).await?,
                ty: self.text_from(ty, work).await?,
                bound_type: None,
                parameter_index: None,
            },
            Expr::Param(number) => ScalarExpr::Param(*number),
            Expr::Func {
                name,
                binding,
                args,
                distinct,
                order_by,
                order_syntax,
                filter,
            } => ScalarExpr::Func {
                name: self.text_from(name, work).await?,
                binding: match binding {
                    Some(binding) => Some(self.binding_from(binding, None, work).await?),
                    None => None,
                },
                args: self.scalar_slots(args.len(), work).await?,
                distinct: *distinct,
                order_by: self.order_slots(order_by, work).await?,
                order_syntax: *order_syntax,
                filter: self.optional_slot(filter.is_some(), work).await?,
            },
            Expr::Array(items) => ScalarExpr::Array(self.scalar_slots(items.len(), work).await?),
            Expr::Row(items) => ScalarExpr::Row(self.scalar_slots(items.len(), work).await?),
            Expr::CompositeRow { items, binding } => ScalarExpr::CompositeRow {
                items: self.scalar_slots(items.len(), work).await?,
                binding: self.composite_binding_from(binding, work).await?,
                bound_type: None,
            },
            Expr::Binary { op, .. } => ScalarExpr::Binary {
                op: *op,
                lhs: self.scalar_slot(work).await?,
                rhs: self.scalar_slot(work).await?,
            },
            Expr::UnaryMinus(_) => ScalarExpr::UnaryMinus(self.scalar_slot(work).await?),
            Expr::Not(_) => ScalarExpr::Not(self.scalar_slot(work).await?),
            Expr::And(items) => ScalarExpr::And(self.scalar_slots(items.len(), work).await?),
            Expr::Or(items) => ScalarExpr::Or(self.scalar_slots(items.len(), work).await?),
            Expr::IsNull { negated, .. } => ScalarExpr::IsNull {
                expr: self.scalar_slot(work).await?,
                negated: *negated,
            },
            Expr::Between { .. } => ScalarExpr::Between {
                expr: self.scalar_slot(work).await?,
                low: self.scalar_slot(work).await?,
                high: self.scalar_slot(work).await?,
            },
            Expr::InList { list, negated, .. } => ScalarExpr::InList {
                expr: self.scalar_slot(work).await?,
                list: self.scalar_slots(list.len(), work).await?,
                negated: *negated,
            },
            Expr::WindowCall {
                name,
                args,
                spec,
                filter,
                modifiers,
            } => ScalarExpr::WindowCall {
                modifiers: *modifiers,
                name: self.text_from(name, work).await?,
                args: self.scalar_slots(args.len(), work).await?,
                spec: self.window_slots(spec, work).await?,
                filter: self.optional_slot(filter.is_some(), work).await?,
            },
            Expr::Case {
                base,
                when,
                else_branch,
            } => ScalarExpr::Case {
                base: self.optional_slot(base.is_some(), work).await?,
                when: map_from!(self, 0..when.len(), work, |_| (
                    ScalarExpr::Star,
                    ScalarExpr::Star
                )),
                else_branch: self.optional_slot(else_branch.is_some(), work).await?,
            },
            Expr::Cast { implicit, ty, .. } => ScalarExpr::Cast {
                implicit: *implicit,
                expr: self.scalar_slot(work).await?,
                ty: self.text_from(ty, work).await?,
            },
            Expr::ScalarSubquery(query) => {
                ScalarExpr::ScalarSubquery(queries.append(query, &self.production()).await?)
            }
            Expr::Exists { body, negated } => ScalarExpr::Exists {
                subquery: queries.append(body, &self.production()).await?,
                negated: *negated,
            },
            Expr::InSubquery { negated, .. } => ScalarExpr::InSubquery {
                expr: self.scalar_slot(work).await?,
                subquery: 0,
                negated: *negated,
            },
        })
    }

    async fn scalar_slots<W: ScalarInputWork>(
        &mut self,
        count: usize,
        work: &mut W,
    ) -> Result<Vec<ScalarExpr>, W::Error> {
        Ok(map_from!(self, 0..count, work, |_| ScalarExpr::Star))
    }

    async fn scalar_slot<W: ScalarInputWork>(
        &mut self,
        work: &mut W,
    ) -> Result<Box<ScalarExpr>, W::Error> {
        Ok(boxed_from!(self, work, ScalarExpr, ScalarExpr::Star))
    }

    async fn optional_slot<W: ScalarInputWork>(
        &mut self,
        present: bool,
        work: &mut W,
    ) -> Result<Option<Box<ScalarExpr>>, W::Error> {
        if present {
            Ok(Some(self.scalar_slot(work).await?))
        } else {
            Ok(None)
        }
    }

    async fn order_slots<W: ScalarInputWork>(
        &mut self,
        orders: &[OrderBy],
        work: &mut W,
    ) -> Result<Vec<ScalarOrder>, W::Error> {
        Ok(map_from!(self, orders.iter(), work, |order| ScalarOrder {
            expr: ScalarExpr::Star,
            descending: order.descending,
            nulls: order.nulls,
        }))
    }

    async fn window_slots<W: ScalarInputWork>(
        &mut self,
        spec: &WindowSpec,
        work: &mut W,
    ) -> Result<ScalarWindowSpec, W::Error> {
        assert!(
            spec.reference.is_none(),
            "named window reference must be resolved before unified-plan lowering"
        );
        Ok(ScalarWindowSpec {
            definition: spec.definition,
            partition_by: self.scalar_slots(spec.partition_by.len(), work).await?,
            order_by: self.order_slots(&spec.order_by, work).await?,
            frame: match &spec.frame {
                Some(frame) => Some(ScalarWindowFrame {
                    mode: frame.mode,
                    start: self.frame_slot(&frame.start, work).await?,
                    end: self.frame_slot(&frame.end, work).await?,
                    between: frame.between,
                    exclusion: frame.exclusion,
                }),
                None => None,
            },
        })
    }

    async fn frame_slot<W: ScalarInputWork>(
        &mut self,
        bound: &FrameBound,
        work: &mut W,
    ) -> Result<ScalarFrameBound, W::Error> {
        self.checkpoint(work).await?;
        Ok(match bound {
            FrameBound::UnboundedPreceding => ScalarFrameBound::UnboundedPreceding,
            FrameBound::UnboundedFollowing => ScalarFrameBound::UnboundedFollowing,
            FrameBound::CurrentRow => ScalarFrameBound::CurrentRow,
            FrameBound::Preceding(_) => ScalarFrameBound::Preceding(self.scalar_slot(work).await?),
            FrameBound::Following(_) => ScalarFrameBound::Following(self.scalar_slot(work).await?),
        })
    }
}
