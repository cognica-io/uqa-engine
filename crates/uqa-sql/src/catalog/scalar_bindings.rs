//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Analyze captured scalar declarations without rebinding, evaluation or routine compilation.

use crate::{
    ast::Expr,
    binding::production::ControlledBindingContext,
    catalog::type_references::TypeReferences,
    plan::ExpressionPlan,
    routines::{native::NativeRoutineReferences, RoutineResolution},
    ColumnType, RowSchema, SQLError, ScalarExpr,
};
use uqa_core::memory::{Produced, ProductionControl};

pub struct NativeScalarContext<'a, 'b> {
    pub subject: &'static str,
    pub routines: &'a dyn RoutineResolution,
    pub references: &'a NativeRoutineReferences<'b>,
    pub types: &'a TypeReferences<'b>,
    pub binding: &'a ControlledBindingContext<'b>,
}

impl NativeScalarContext<'_, '_> {
    fn invalid(&self, detail: &str) -> SQLError {
        SQLError::Internal(format!("invalid native {} binding: {detail}", self.subject))
    }
}

/// Preserve the stored expression and its selected identities; return only its admitted result type.
pub fn expression_type(
    expression: &Expr,
    schema: &RowSchema,
    context: &NativeScalarContext<'_, '_>,
    control: &ProductionControl<'_>,
) -> Result<Option<Produced<ColumnType>>, SQLError> {
    analyze(expression, schema, context, None, control)
}

fn analyze(
    expression: &Expr,
    schema: &RowSchema,
    context: &NativeScalarContext<'_, '_>,
    boolean_context: Option<&str>,
    control: &ProductionControl<'_>,
) -> Result<Option<Produced<ColumnType>>, SQLError> {
    crate::schema::dependencies::walk_schema_expr_with_control(expression, control, &mut |node| {
        if matches!(
            node,
            Expr::Param(_) | Expr::Default | Expr::InternalColumn(_) | Expr::WindowCall { .. }
        ) {
            return Err(context.invalid("non-scalar expression"));
        }
        match node {
            Expr::Cast { ty, .. } | Expr::TypedLiteral { ty, .. } => {
                context.types.stored_name(ty)?;
            }
            Expr::CompositeRow { binding, .. } => context.types.stored_name(&binding.ty)?,
            Expr::RetainedLiteral(literal) => {
                if let Some(ty) = &literal.0.ty {
                    context.types.column_type(ty)?;
                }
            }
            _ => (),
        }
        Ok(())
    })?;
    context.references.expression(expression)?;
    let plan = ExpressionPlan::lower_borrowed_for_rewrite(expression, control)?;
    if !plan.queries().is_empty() {
        return Err(context.invalid("query-valued expression"));
    }
    plan.scalar().try_visit(&mut |node| {
        control.check()?;
        if let ScalarExpr::Func { name, binding, .. } = node {
            if crate::semantics::is_builtin_aggregate_call(name, binding.as_ref())
                || context
                    .routines
                    .has_registered_aggregate_function_with_control(name, control)?
            {
                return Err(context.invalid("aggregate expression"));
            }
        }
        Ok(true)
    })?;
    if crate::semantics::sets::validation::expression_may_return_set_with_control(
        context.routines,
        context.routines,
        plan.scalar(),
        schema,
        &[],
        control,
    )? {
        return Err(context.invalid("set-valued expression"));
    }
    crate::binding::analyze_stored_scalar_with_control(
        context.routines,
        plan.scalar(),
        schema,
        context.binding.copy_with_control(control)?,
        boolean_context,
        control,
    )
}

/// A condition accepts Boolean domains and SQL NULL without evaluating its stored expression.
pub fn validate_boolean(
    expression: &Expr,
    schema: &RowSchema,
    context: &NativeScalarContext<'_, '_>,
    control: &ProductionControl<'_>,
) -> Result<(), SQLError> {
    analyze(expression, schema, context, Some(context.subject), control).map(|_| ())
}
