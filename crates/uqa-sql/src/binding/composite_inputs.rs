//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Preserve prepared composite attribute identities using the same scopes as ordinary SQL binding.

use super::{BindingContext, SchemaScope};
use crate::{
    plan::UnifiedPlan, routines::RoutineResolution, RowSchema, SQLError, SQLParam, ScalarExpr,
};

pub(crate) fn retain_composite_inputs(
    routines: &dyn RoutineResolution,
    plan: &mut UnifiedPlan,
    params: &[SQLParam],
    context: &BindingContext<'_>,
) -> Result<(), SQLError> {
    let mut has_composite_input = false;
    plan.visit_scalar_expressions(&mut |expression| expression.visit(&mut |node| {
        has_composite_input |= matches!(node, ScalarExpr::Func { binding: Some(binding), .. } if binding.dispatch == Some(crate::ast::FunctionDispatch::FieldSelect));
        has_composite_input |= matches!(node, ScalarExpr::Cast { expr, .. } if matches!(expr.as_ref(), ScalarExpr::Row(_)));
    }));
    if !has_composite_input {
        return Ok(());
    }
    let mut scope = SchemaScope::for_analysis(context)?;
    scope.scalar_binding = super::ScalarBindingMode::CompositeInputs;
    scope.preserve_syntax_shape = true;
    scope.bind_statement_parameters(routines, plan, params, None)
}

impl SchemaScope {
    pub(super) fn retain_composite_inputs_in_scope(
        &mut self,
        routines: &dyn RoutineResolution,
        expression: &mut ScalarExpr,
        schema: &RowSchema,
        subqueries: &[crate::plan::QueryPlan],
        params: &[SQLParam],
    ) -> Result<(), SQLError> {
        super::stored_types::bind_scalar_type_identities(expression, &mut |name| {
            routines.resolve_type_name(name)
        })?;
        let resolver = self.query_function_type_resolver_for_subqueries(
            routines,
            std::slice::from_ref(expression),
            schema,
            subqueries,
            params,
        )?;
        let mut failure = None;
        crate::plan::rewrite_scalar_expression(expression, &mut |node| {
            if failure.is_none() {
                if let Err(error) = crate::type_resolution::composite_rows::bind_stored_row(
                    node, schema, params, &resolver,
                ) {
                    failure = Some(error);
                } else if let Err(error) = retain_field(node, schema, params, &resolver) {
                    failure = Some(error);
                }
            }
        });
        failure.map_or(Ok(()), Err)
    }
}

fn retain_field(
    expression: &mut ScalarExpr,
    schema: &RowSchema,
    params: &[SQLParam],
    types: &dyn crate::type_resolution::FunctionTypeResolver,
) -> Result<(), SQLError> {
    let ScalarExpr::Func {
        binding: Some(binding),
        args,
        ..
    } = expression
    else {
        return Ok(());
    };
    if binding.dispatch != Some(crate::ast::FunctionDispatch::FieldSelect)
        || binding.composite_field.is_some()
    {
        return Ok(());
    }
    let [base, ScalarExpr::Literal(uqa_core::Value::Str(name))] = args.as_slice() else {
        return Ok(());
    };
    let Some(ty) = crate::type_resolution::scalar_type_with_resolver(base, schema, params, types)?
    else {
        return Ok(());
    };
    let mut base = &ty;
    while let crate::ColumnType::Domain { base: nested, .. } = base {
        base = nested;
    }
    let crate::ColumnType::Composite(reference) = base else {
        return Ok(());
    };
    let descriptor = crate::expr::composites::descriptor(types.composite_types(), reference.oid)?;
    if let Some((_, attribute)) = descriptor.attribute(name) {
        binding.composite_field = Some(Box::new(crate::ast::CompositeFieldBinding {
            type_oid: reference.oid,
            number: attribute.number,
            result_type: attribute.ty.clone(),
            dropped: false,
            changed_type: None,
        }));
    }
    Ok(())
}
