//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Expression-kind restrictions at the ordered SQL binding boundary.

use super::{error, Preparation, RowSchema, SQLError, ScalarExpr};
use crate::ast::FunctionBinding;
use crate::plan::AggregateClassifier;

#[derive(Clone, Copy)]
pub(super) enum SchemaExpressionKind {
    TypeTransform,
    DomainCheck,
    RoutineDefault,
}

impl SchemaExpressionKind {
    fn singular(self) -> &'static str {
        match self {
            Self::TypeTransform => "transform expression",
            Self::DomainCheck => "check constraint",
            Self::RoutineDefault => "DEFAULT expression",
        }
    }

    fn plural(self) -> &'static str {
        match self {
            Self::TypeTransform => "transform expressions",
            Self::DomainCheck => "check constraints",
            Self::RoutineDefault => "DEFAULT expressions",
        }
    }
}

#[derive(Clone, Copy)]
pub(super) struct SchemaExpressionContext<'a> {
    pub(super) aggregates: &'a dyn AggregateClassifier,
    pub(super) kind: SchemaExpressionKind,
}

impl Preparation<'_> {
    pub(super) fn check_schema_subquery(&self, expression: &ScalarExpr) -> Result<(), SQLError> {
        let Some(context) = self.schema_expression else {
            return Ok(());
        };
        if matches!(
            expression,
            ScalarExpr::ScalarSubquery(_)
                | ScalarExpr::Exists { .. }
                | ScalarExpr::InSubquery { .. }
        ) {
            return Err(error(
                "0A000",
                format!("cannot use subquery in {}", context.kind.singular()),
            ));
        }
        Ok(())
    }

    pub(super) fn check_schema_window(
        &self,
        name: &str,
        call: (&[ScalarExpr], bool, crate::ast::WindowCallModifiers),
        input: &RowSchema,
    ) -> Result<(), SQLError> {
        let Some(context) = self.schema_expression else {
            return Ok(());
        };
        super::super::analysis::validate_window_function(
            self.routines,
            name,
            call,
            input,
            &[],
            self.routines,
        )?;
        Err(error(
            "42P20",
            format!(
                "window functions are not allowed in {}",
                context.kind.plural()
            ),
        ))
    }

    pub(super) fn check_schema_function(
        &self,
        expression: &ScalarExpr,
        selected: Option<&FunctionBinding>,
        input: &RowSchema,
    ) -> Result<(), SQLError> {
        let Some(context) = self.schema_expression else {
            return Ok(());
        };
        let ScalarExpr::Func { name, args, .. } = expression else {
            unreachable!("schema expression call context");
        };
        let scalar = selected
            .map(|binding| self.routines.is_scalar_function_binding(binding))
            .transpose()?
            .unwrap_or(false);
        if !scalar
            && (crate::semantics::is_builtin_aggregate_call(name, selected)
                || context.aggregates.is_registered_aggregate(name))
        {
            return Err(error(
                "42803",
                format!(
                    "aggregate functions are not allowed in {}",
                    context.kind.plural()
                ),
            ));
        }
        if crate::semantics::sets::validation::function_may_return_set(
            self.routines,
            self.routines,
            name,
            selected,
            args,
            input,
            &[],
        )? {
            return Err(error(
                "0A000",
                format!(
                    "set-returning functions are not allowed in {}",
                    context.kind.plural()
                ),
            ));
        }
        Ok(())
    }
}
