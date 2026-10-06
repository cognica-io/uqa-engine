//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `transformWindowDefinitions`' checks of window frames: the `ORDER BY` a frame mode requires, and the type and variables of each frame offset.

use super::super::{QueryBlockPlan, QueryPlan, SQLError, SQLParam, ScalarExpr, SchemaScope};
use crate::ast::{ColumnType, FrameMode};
use crate::routines::RoutineResolution;
use crate::{RowSchema, ScalarFrameBound, ScalarWindowSpec};

/// What a query block's window frames are checked against: `source` resolves every reference, including the outer ones it overlays.
pub(super) struct WindowFrameScope<'a> {
    pub(super) source: &'a RowSchema,
    pub(super) subqueries: &'a [QueryPlan],
    pub(super) params: &'a [SQLParam],
}

impl SchemaScope {
    /// Check the frames of the windows that a query block's target list and then its `ORDER BY` use, in the order they appear.
    pub(super) fn validate_window_frames(
        &mut self,
        engine: &dyn RoutineResolution,
        block: &QueryBlockPlan,
        scope: &WindowFrameScope<'_>,
    ) -> Result<(), SQLError> {
        let mut specs = Vec::new();
        for expression in block
            .projections
            .iter()
            .map(|projection| &projection.expr)
            .chain(block.order_by.iter().map(|order| &order.expr))
        {
            expression.visit(&mut |part| {
                if let ScalarExpr::WindowCall { spec, .. } = part {
                    if spec.frame.is_some() {
                        specs.push(spec.clone());
                    }
                }
            });
        }
        for spec in &specs {
            self.validate_window_frame(engine, spec, scope)?;
        }
        Ok(())
    }

    fn validate_window_frame(
        &mut self,
        engine: &dyn RoutineResolution,
        spec: &ScalarWindowSpec,
        scope: &WindowFrameScope<'_>,
    ) -> Result<(), SQLError> {
        let Some(frame) = &spec.frame else {
            return Ok(());
        };
        let offsets = [&frame.start, &frame.end]
            .into_iter()
            .filter_map(|bound| match bound {
                ScalarFrameBound::Preceding(offset) | ScalarFrameBound::Following(offset) => {
                    Some(offset.as_ref())
                }
                ScalarFrameBound::UnboundedPreceding
                | ScalarFrameBound::UnboundedFollowing
                | ScalarFrameBound::CurrentRow => None,
            })
            .collect::<Vec<_>>();
        if frame.mode == FrameMode::Range && !offsets.is_empty() && spec.order_by.len() != 1 {
            return Err(windowing_error(
                "RANGE with offset PRECEDING/FOLLOWING requires exactly one ORDER BY column",
            ));
        }
        if frame.mode == FrameMode::Groups && spec.order_by.is_empty() {
            return Err(windowing_error("GROUPS mode requires an ORDER BY clause"));
        }
        let construct = match frame.mode {
            FrameMode::Rows => "ROWS",
            FrameMode::Range => "RANGE",
            FrameMode::Groups => "GROUPS",
        };
        for offset in offsets {
            let offset_type = self.known_type(engine, offset, scope)?;
            if frame.mode == FrameMode::Range {
                let order = &spec.order_by[0].expr;
                let order_type = self.known_type(engine, order, scope)?;
                crate::range_frame_offset_type(order_type.as_ref(), offset_type.as_ref())?;
            } else if let Some(ty) = offset_type {
                if !crate::assignment_type_compatible(&ty, &ColumnType::BigInteger) {
                    return Err(SQLError::Routine {
                        sqlstate: "42804".into(),
                        message: format!(
                            "argument of {construct} must be type bigint, not type {}",
                            ty.regtype_name()
                        ),
                    });
                }
            }
            if references_local_column(offset, scope.source) {
                return Err(SQLError::Routine {
                    sqlstate: "42P10".into(),
                    message: format!("argument of {construct} must not contain variables"),
                });
            }
        }
        Ok(())
    }

    /// The type of an expression, or `None` when it is an `unknown` literal or an untyped parameter.
    fn known_type(
        &mut self,
        engine: &dyn RoutineResolution,
        expression: &ScalarExpr,
        scope: &WindowFrameScope<'_>,
    ) -> Result<Option<ColumnType>, SQLError> {
        let resolver = self.query_function_type_resolver(
            engine,
            expression,
            scope.source,
            scope.subqueries,
            scope.params,
            Some(scope.source),
        )?;
        let ty =
            crate::scalar_type_with_resolver(expression, scope.source, scope.params, &resolver)?;
        Ok(crate::effective_overload_argument_type_with_params(
            expression,
            ty,
            scope.params,
        ))
    }
}

/// Whether an expression refers to a column of the query block itself rather than of an enclosing query, as `contain_vars_of_level(n, 0)` asks.
pub(in crate::binding) fn references_local_column(
    expression: &ScalarExpr,
    source: &RowSchema,
) -> bool {
    let mut found = false;
    expression.visit(&mut |part| {
        found |= match part {
            ScalarExpr::Column(column) => source.resolves_local_column(None, column),
            ScalarExpr::QualifiedColumn { qualifier, column } => {
                source.resolves_local_column(Some(qualifier), column)
            }
            ScalarExpr::Position(_) | ScalarExpr::InternalColumn(_) => true,
            _ => false,
        };
    });
    found
}

fn windowing_error(message: &str) -> SQLError {
    SQLError::Routine {
        sqlstate: "42P20".into(),
        message: message.into(),
    }
}
