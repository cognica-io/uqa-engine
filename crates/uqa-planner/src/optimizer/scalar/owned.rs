//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Native scalar planning transfers constructor owners through ordered rewrites and replacements.

use super::{
    conditional, constants, vector_thresholds, OptimizerConfig, SQLError, ScalarExpr,
    ScalarProduction, Value,
};
use uqa_sql::{plan::OwnedScalarExpr, ColumnType};

mod calls;
mod finish;

pub(in crate::optimizer) fn optimize(
    expression: OwnedScalarExpr,
    config: &OptimizerConfig<'_>,
) -> Result<OwnedScalarExpr, SQLError> {
    optimize_in(expression, ScalarProduction::admitted(config))
}

pub(super) fn optimize_in(
    expression: OwnedScalarExpr,
    production: ScalarProduction<'_, '_>,
) -> Result<OwnedScalarExpr, SQLError> {
    Context { production }.slot(expression)
}

struct Context<'a, 'scope> {
    production: ScalarProduction<'a, 'scope>,
}

impl Context<'_, '_> {
    fn slot(&self, expression: OwnedScalarExpr) -> Result<OwnedScalarExpr, SQLError> {
        let output = self.scalar(expression)?;
        if self.production.config.enable_vector_threshold_merge {
            vector_thresholds::merge_owned(
                output,
                &self.production.storage,
                &self.production.config.production,
            )
        } else {
            Ok(output)
        }
    }

    fn scalar(&self, expression: OwnedScalarExpr) -> Result<OwnedScalarExpr, SQLError> {
        self.production.config.production.check()?;
        expression.rewrite(&self.production.storage, |expression| match &*expression {
            ScalarExpr::Collated { .. } => self.collated(expression),
            ScalarExpr::Func { .. } => self.call(expression),
            _ => self.noncall(expression, None),
        })
    }

    fn noncall(
        &self,
        expression: OwnedScalarExpr,
        result_type: Option<&ColumnType>,
    ) -> Result<OwnedScalarExpr, SQLError> {
        let control = &self.production.storage;
        let integer_syntax = matches!(&*expression, ScalarExpr::Literal(Value::Int(_)));
        let row_syntax = matches!(&*expression, ScalarExpr::Row(_));
        let expression = match &*expression {
            ScalarExpr::Binary { lhs, rhs, .. } => {
                let row_comparison =
                    matches!((&**lhs, &**rhs), (ScalarExpr::Row(_), ScalarExpr::Row(_)));
                expression.map_children(control, |_, child| {
                    if row_comparison && matches!(&*child, ScalarExpr::Row(_)) {
                        child.map_children(control, |_, item| self.scalar(item))
                    } else {
                        self.scalar(child)
                    }
                })?
            }
            ScalarExpr::Not(_) => {
                let expression = expression.map_child(0, control, |child| self.scalar(child))?;
                if self.production.config.enable_boolean_simplify
                    && matches!(&*expression, ScalarExpr::Not(child) if matches!(child.as_ref(), ScalarExpr::Not(_)))
                {
                    expression.select_child(0).select_child(0)
                } else {
                    expression
                }
            }
            ScalarExpr::And(_) => conditional::optimize_owned_boolean(
                expression,
                true,
                self.production.config,
                control,
                |child| self.scalar(child),
            )?,
            ScalarExpr::Or(_) => conditional::optimize_owned_boolean(
                expression,
                false,
                self.production.config,
                control,
                |child| self.scalar(child),
            )?,
            ScalarExpr::Case { .. } => conditional::optimize_owned_case(
                expression,
                self.production.config,
                control,
                |child| self.scalar(child),
            )?,
            ScalarExpr::WindowCall { .. } => self.window(expression)?,
            ScalarExpr::Array(_)
            | ScalarExpr::Row(_)
            | ScalarExpr::UnaryMinus(_)
            | ScalarExpr::IsNull { .. }
            | ScalarExpr::Between { .. }
            | ScalarExpr::InList { .. }
            | ScalarExpr::Cast { .. }
            | ScalarExpr::InSubquery { .. } => {
                expression.map_children(control, |_, child| self.scalar(child))?
            }
            _ => expression,
        };
        self.finish(expression, integer_syntax, row_syntax, result_type)
    }

    fn window(&self, mut expression: OwnedScalarExpr) -> Result<OwnedScalarExpr, SQLError> {
        let ScalarExpr::WindowCall {
            args, spec, filter, ..
        } = &*expression
        else {
            unreachable!("window traversal")
        };
        let argument_count = args.len();
        let partition_count = spec.partition_by.len();
        let has_filter = filter.is_some();
        let last = expression.child_count() - usize::from(has_filter);
        let control = &self.production.storage;
        for index in argument_count..argument_count + partition_count {
            expression = expression.map_child(index, control, |child| self.scalar(child))?;
        }
        for index in argument_count + partition_count..last {
            expression = expression.map_child(index, control, |child| self.slot(child))?;
        }
        for index in 0..argument_count {
            expression = expression.map_child(index, control, |child| self.scalar(child))?;
        }
        if has_filter {
            expression = expression.map_child(last, control, |child| self.scalar(child))?;
        }
        Ok(expression)
    }
}
