//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Analyze captured scalar declarations without rebinding, evaluation or routine compilation.

use crate::{
    ast::Expr,
    binding::production::ControlledBindingContext,
    catalog::{
        type_references::TypeReferences,
        work::{inline, ReferenceWork, Work},
    },
    ir::{
        children::Scope,
        observation::{visit_in, Decision},
    },
    plan::ExpressionPlan,
    routines::{native::NativeRoutineReferences, RoutineResolution},
    schema::dependencies::{observe_schema_expression, SchemaObserver},
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
    inline(expression_type_with_work(
        expression,
        schema,
        context,
        control,
        &mut (),
    ))
}

pub async fn expression_type_with_work(
    expression: &Expr,
    schema: &RowSchema,
    context: &NativeScalarContext<'_, '_>,
    control: &ProductionControl<'_>,
    work: &mut (impl Work + Send),
) -> Result<Option<Produced<ColumnType>>, SQLError> {
    analyze(expression, schema, context, None, control, work).await
}

async fn analyze<W: Work + Send>(
    expression: &Expr,
    schema: &RowSchema,
    context: &NativeScalarContext<'_, '_>,
    boolean_context: Option<&str>,
    control: &ProductionControl<'_>,
    work: &mut W,
) -> Result<Option<Produced<ColumnType>>, SQLError> {
    let mut observer = context;
    observe_schema_expression(expression, control, work, &mut observer).await?;
    context
        .references
        .expression_with_work(expression, work)
        .await?;
    let scalar = ExpressionPlan::lower_stored_scalar_with_work(expression, control, work).await?;
    visit_in(&scalar, control, work, Scope::Complete, &mut |node| {
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
        Ok(Decision::Descend)
    })
    .await?;
    if crate::semantics::sets::validation::expression_may_return_set_with_work(
        context.routines,
        context.routines,
        &scalar,
        schema,
        &[],
        control,
        work,
    )
    .await?
    {
        return Err(context.invalid("set-valued expression"));
    }
    let ty = crate::binding::analyze_stored_scalar_with_control(
        context.routines,
        &scalar,
        schema,
        context.binding.copy_with_control(control)?,
        boolean_context,
        control,
    )?;
    if W::MAY_SUSPEND && control.budget().is_some() {
        scalar
            .retire_from(control, &mut ReferenceWork { control, work })
            .await?;
    }
    Ok(ty)
}

impl SchemaObserver for &NativeScalarContext<'_, '_> {
    async fn expression<W: Work + Send>(
        &mut self,
        node: &Expr,
        work: &mut W,
    ) -> Result<(), SQLError> {
        match node {
            Expr::Param(_) | Expr::Default | Expr::InternalColumn(_) | Expr::WindowCall { .. } => {
                Err(self.invalid("non-scalar expression"))
            }
            Expr::Cast { ty, .. } | Expr::TypedLiteral { ty, .. } => {
                self.types.stored_name_with_work(ty, work).await
            }
            Expr::CompositeRow { binding, .. } => {
                self.types.stored_name_with_work(&binding.ty, work).await
            }
            Expr::RetainedLiteral(literal) => match &literal.0.ty {
                Some(ty) => self.types.column_type_with_work(ty, work).await,
                None => Ok(()),
            },
            _ => Ok(()),
        }
    }
}

/// A condition accepts Boolean domains and SQL NULL without evaluating its stored expression.
pub fn validate_boolean(
    expression: &Expr,
    schema: &RowSchema,
    context: &NativeScalarContext<'_, '_>,
    control: &ProductionControl<'_>,
) -> Result<(), SQLError> {
    inline(validate_boolean_with_work(
        expression,
        schema,
        context,
        control,
        &mut (),
    ))
}

pub async fn validate_boolean_with_work(
    expression: &Expr,
    schema: &RowSchema,
    context: &NativeScalarContext<'_, '_>,
    control: &ProductionControl<'_>,
    work: &mut (impl Work + Send),
) -> Result<(), SQLError> {
    analyze(
        expression,
        schema,
        context,
        Some(context.subject),
        control,
        work,
    )
    .await
    .map(|_| ())
}
