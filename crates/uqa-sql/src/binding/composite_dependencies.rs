//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Attribute identities selected from named composite values, resolved with the ordinary SQL scopes and type rules.

use super::{BindingContext, SchemaScope};
use crate::ast::{ColumnType, FunctionDispatch};
use crate::catalog::dependencies::ObjectAddress;
use crate::plan::QueryPlan;
use crate::routines::RoutineResolution;
use crate::schema::ScalarTypeSchema;
use crate::type_resolution::{scalar_type_with_resolver, FunctionTypeResolver};
use crate::{RowSchema, SQLError, SQLParam, ScalarExpr};
use uqa_core::Value;

/// References of field selections in one expression. Anonymous rows and whole-row relation projections do not introduce a named composite attribute dependency.
pub fn expression_composite_dependencies(
    expression: &ScalarExpr,
    schema: &dyn ScalarTypeSchema,
    params: &[SQLParam],
    types: &dyn FunctionTypeResolver,
) -> Result<Vec<ObjectAddress>, SQLError> {
    let mut references = Vec::new();
    let mut failure = None;
    expression.visit(&mut |node| {
        if failure.is_some() {
            return;
        }
        let ScalarExpr::Func { binding, args, .. } = node else {
            return;
        };
        if binding.as_ref().and_then(|binding| binding.dispatch)
            != Some(FunctionDispatch::FieldSelect)
        {
            return;
        }
        let [base, ScalarExpr::Literal(Value::Str(field))] = args.as_slice() else {
            return;
        };
        let result = (|| {
            let Some(mut ty) = scalar_type_with_resolver(base, schema, params, types)? else {
                return Ok(None);
            };
            while let ColumnType::Domain { base, .. } = ty {
                ty = *base;
            }
            let ColumnType::Composite(reference) = ty else {
                return Ok(None);
            };
            let descriptor =
                crate::expr::composites::descriptor(types.composite_types(), reference.oid)?;
            Ok(descriptor.attribute(field).map(|(_, attribute)| {
                ObjectAddress::column(descriptor.relation_oid, i32::from(attribute.number))
            }))
        })();
        match result {
            Ok(Some(reference)) => references.push(reference),
            Ok(None) => {}
            Err(error) => failure = Some(error),
        }
    });
    failure.map_or(Ok(references), Err)
}

/// Rebind a private query copy against the supplied catalog to collect field dependencies even for definitions written before attribute removal was supported. No persistent representation or input constant is changed.
pub fn query_composite_dependencies(
    routines: &dyn RoutineResolution,
    query: &QueryPlan,
    params: &[SQLParam],
    context: &BindingContext<'_>,
    outer: Option<&RowSchema>,
) -> Result<Vec<ObjectAddress>, SQLError> {
    let mut contains_field = false;
    query.visit_scalar_expressions(&mut |expression| {
        expression.visit(&mut |node| {
            contains_field |= matches!(node, ScalarExpr::Func { binding: Some(binding), .. }
            if binding.dispatch == Some(FunctionDispatch::FieldSelect));
        })
    });
    if !contains_field {
        return Ok(Vec::new());
    }
    let mut scope = SchemaScope::for_analysis(context)?;
    scope.composite_dependencies = Some(Vec::new());
    scope.bind_query_routines_for_storage(routines, &mut query.clone(), params, outer)?;
    Ok(scope.composite_dependencies.unwrap_or_default())
}

/// Resolve scalar subqueries with their own source scopes and the surrounding expression's correlated row scope.
pub fn expression_plan_composite_dependencies(
    routines: &dyn RoutineResolution,
    plan: &crate::plan::ExpressionPlan,
    params: &[SQLParam],
    context: &BindingContext<'_>,
    schema: &RowSchema,
) -> Result<Vec<ObjectAddress>, SQLError> {
    let mut scope = SchemaScope::for_analysis(context)?;
    scope.composite_dependencies = Some(Vec::new());
    scope.preserve_syntax_shape = true;
    scope.stored_expression_outer = Some(schema.clone());
    let mut plan = plan.clone();
    for subquery in &mut plan.subqueries {
        scope.bind_query_routines_for_storage(routines, subquery, params, Some(schema))?;
    }
    scope.bind_scalar_routines_for_storage(
        routines,
        &mut plan.scalar,
        schema,
        &plan.subqueries,
        params,
    )?;
    Ok(scope.composite_dependencies.unwrap_or_default())
}

impl SchemaScope {
    pub(super) fn record_composite_dependencies(
        &mut self,
        routines: &dyn RoutineResolution,
        expression: &ScalarExpr,
        schema: &RowSchema,
        subqueries: &[QueryPlan],
        params: &[SQLParam],
    ) -> Result<(), SQLError> {
        if self.composite_dependencies.is_none() {
            return Ok(());
        }
        let resolver = self.query_function_type_resolver_for_subqueries(
            routines,
            std::slice::from_ref(expression),
            schema,
            subqueries,
            params,
        )?;
        let references = expression_composite_dependencies(expression, schema, params, &resolver)?;
        if let Some(dependencies) = &mut self.composite_dependencies {
            dependencies.extend(references);
        }
        Ok(())
    }
}

/// Resolve the typed parameters and source scopes of one stored SQL-standard routine statement without executing its expressions.
pub fn routine_statement_composite_dependencies(
    routines: &dyn RoutineResolution,
    definition: &crate::ast::CreateFunction,
    statement: &crate::ast::Statement,
    context: &BindingContext<'_>,
) -> Result<Vec<ObjectAddress>, SQLError> {
    let mut plan = crate::plan::UnifiedPlan::lower_with(statement.clone(), &|name: &str| {
        routines.has_registered_aggregate_function(name)
    });
    let mut contains_field = false;
    plan.rewrite_scalar_expressions(&mut |node| {
        contains_field |= matches!(node, ScalarExpr::Func { binding: Some(binding), .. }
            if binding.dispatch == Some(FunctionDispatch::FieldSelect));
    });
    if !contains_field {
        return Ok(Vec::new());
    }
    super::stored_routines::mark_catalog_statement_relations_bound(&mut plan)?;
    let params = crate::routines::body_parameters::sql_body_parameters(definition)
        .into_iter()
        .map(|parameter| {
            routines.resolve_type_name(&parameter.type_name).map(|ty| {
                ty.map_or_else(
                    || SQLParam::scalar(Value::Null),
                    |ty| SQLParam::typed_scalar(Value::Null, ty),
                )
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    let parameters =
        crate::routines::body_parameters::sql_body_parameter_scope(definition, &params)?;
    let mut scope = SchemaScope::for_analysis(context)?;
    scope.routine_parameters = Some(parameters.clone());
    scope.composite_dependencies = Some(Vec::new());
    scope.preserve_syntax_shape = true;
    scope.bind_statement_parameters(routines, &mut plan, &params, Some(parameters.schema()))?;
    Ok(scope.composite_dependencies.unwrap_or_default())
}

/// Resolve the source and transition scopes of a stored rule action.
pub fn statement_composite_dependencies(
    routines: &dyn RoutineResolution,
    statement: &crate::ast::Statement,
    context: &BindingContext<'_>,
    outer: &RowSchema,
) -> Result<Vec<ObjectAddress>, SQLError> {
    let mut plan = crate::plan::UnifiedPlan::lower_with(statement.clone(), &|name: &str| {
        routines.has_registered_aggregate_function(name)
    });
    let mut contains_field = false;
    plan.rewrite_scalar_expressions(&mut |node| {
        contains_field |= matches!(node, ScalarExpr::Func { binding: Some(binding), .. }
            if binding.dispatch == Some(FunctionDispatch::FieldSelect));
    });
    if !contains_field {
        return Ok(Vec::new());
    }
    super::stored_routines::mark_catalog_statement_relations_bound(&mut plan)?;
    let mut scope = SchemaScope::for_analysis(context)?;
    scope.composite_dependencies = Some(Vec::new());
    scope.preserve_syntax_shape = true;
    scope.bind_statement_parameters(routines, &mut plan, &[], Some(outer))?;
    Ok(scope.composite_dependencies.unwrap_or_default())
}

#[cfg(test)]
mod tests;
