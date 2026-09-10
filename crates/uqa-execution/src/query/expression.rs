//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Shared expression evaluation for physical filters, projections, and sorts.

use crate::scalar::plan::{eval_physical_scalar, PhysicalEvalContext, QueryExpressionContext};
use crate::{
    ExecResult, ExpressionEvaluator, FunctionTypeResolver, RowSchemaExecution, ScalarExpr,
    SharedExpressionEvaluator,
};
use std::sync::Arc;
use uqa_core::Value;
use uqa_sql::{expr::RowLookup, SQLError, SQLParam};

/// Scalar execution services that can intercept a function requiring its unevaluated argument expressions.
pub trait ScalarExpressionContext: QueryExpressionContext {
    fn intercept_function(
        &self,
        name: &str,
        args: &[ScalarExpr],
        row: &dyn RowLookup,
        evaluate: &mut dyn FnMut(&ScalarExpr) -> Result<Value, SQLError>,
    ) -> Result<Option<Value>, SQLError>;
}

pub struct ScopedExpressionEvaluator<'a> {
    context: Arc<dyn ScalarExpressionContext + 'a>,
    params: &'a [SQLParam],
    cancellation: uqa_core::CancellationToken,
}

impl<'a> ScopedExpressionEvaluator<'a> {
    pub fn shared(
        context: Arc<dyn ScalarExpressionContext + 'a>,
        params: &'a [SQLParam],
        cancellation: uqa_core::CancellationToken,
    ) -> SharedExpressionEvaluator<'a> {
        Arc::new(Self {
            context,
            params,
            cancellation,
        })
    }

    fn evaluate_physical_scoped(
        &self,
        expression: &ScalarExpr,
        schema: &crate::RowSchema,
        row: &crate::PhysicalRow,
    ) -> ExecResult<Value> {
        self.cancellation.check().map_err(SQLError::from)?;
        let view = schema.view(row);
        let hook = self.context.as_ref();
        let context = PhysicalEvalContext::from_row_lookup(&view, self.params)
            .with_row_schema(schema)
            .with_function_hook(hook)
            .with_subquery_runner(hook)
            .with_physical_outer_row(schema, row);
        if let ScalarExpr::Func { name, args, .. } = expression {
            let mut evaluate = |expr: &ScalarExpr| {
                eval_physical_scalar(expr, self.context.subquery_plans(), &context)
            };
            if let Some(value) =
                self.context
                    .intercept_function(name, args, &view, &mut evaluate)?
            {
                return Ok(value);
            }
        }
        Ok(eval_physical_scalar(
            expression,
            self.context.subquery_plans(),
            &context,
        )?)
    }
}

impl ExpressionEvaluator for ScopedExpressionEvaluator<'_> {
    fn evaluate(&self, expression: &ScalarExpr, row: &dyn RowLookup) -> ExecResult<Value> {
        self.cancellation.check().map_err(SQLError::from)?;
        let hook = self.context.as_ref();
        let context = PhysicalEvalContext::from_row_lookup(row, self.params)
            .with_function_hook(hook)
            .with_subquery_runner(hook);
        if let ScalarExpr::Func { name, args, .. } = expression {
            let mut evaluate = |expr: &ScalarExpr| {
                eval_physical_scalar(expr, self.context.subquery_plans(), &context)
            };
            if let Some(value) = self
                .context
                .intercept_function(name, args, row, &mut evaluate)?
            {
                return Ok(value);
            }
        }
        Ok(eval_physical_scalar(
            expression,
            self.context.subquery_plans(),
            &context,
        )?)
    }

    fn evaluate_physical(
        &self,
        expression: &ScalarExpr,
        schema: &crate::RowSchema,
        row: &crate::PhysicalRow,
    ) -> ExecResult<Value> {
        self.evaluate_physical_scoped(expression, schema, row)
    }

    fn parameters(&self) -> &[SQLParam] {
        self.params
    }

    fn expression_type(
        &self,
        expression: &ScalarExpr,
        schema: &crate::RowSchema,
    ) -> Result<Option<uqa_sql::ast::ColumnType>, SQLError> {
        crate::scalar_type_with_resolver(expression, schema, self.params, self)
    }

    fn bind_type_introspection(
        &self,
        expression: ScalarExpr,
        schema: &crate::RowSchema,
    ) -> ScalarExpr {
        crate::bind_type_introspection_with_resolver(expression, schema, self.params, self)
    }
}

impl FunctionTypeResolver for ScopedExpressionEvaluator<'_> {
    fn has_untyped_function(&self, name: &str) -> bool {
        self.context.has_untyped_function(name)
    }

    fn resolve_type_name(&self, name: &str) -> Result<Option<uqa_sql::ast::ColumnType>, SQLError> {
        FunctionTypeResolver::resolve_type_name(self.context.as_ref(), name)
    }

    fn resolve_function_type(
        &self,
        name: &str,
        binding: Option<&uqa_sql::ast::FunctionBinding>,
        argument_names: &[Option<String>],
        argument_types: &[Option<uqa_sql::ast::ColumnType>],
        explicit_variadic: bool,
    ) -> Result<Option<uqa_sql::ast::ColumnType>, SQLError> {
        self.context.resolve_function_type(
            name,
            binding,
            argument_names,
            argument_types,
            explicit_variadic,
        )
    }

    fn resolve_function_overload(
        &self,
        name: &str,
        binding: Option<&uqa_sql::ast::FunctionBinding>,
        argument_names: &[Option<String>],
        argument_types: &[Option<uqa_sql::ast::ColumnType>],
        explicit_variadic: bool,
    ) -> Result<Option<crate::ResolvedFunctionOverload>, SQLError> {
        self.context.resolve_function_overload(
            name,
            binding,
            argument_names,
            argument_types,
            explicit_variadic,
        )
    }

    fn is_scalar_function_binding(
        &self,
        binding: &uqa_sql::ast::FunctionBinding,
    ) -> Result<bool, SQLError> {
        self.context.is_scalar_function_binding(binding)
    }

    fn resolve_function_overload_with_builtins(
        &self,
        name: &str,
        binding: Option<&uqa_sql::ast::FunctionBinding>,
        argument_names: &[Option<String>],
        argument_types: &[Option<uqa_sql::ast::ColumnType>],
        explicit_variadic: bool,
        builtins: &[crate::BuiltinFunctionOverload],
    ) -> Result<Option<crate::ResolvedFunctionOverload>, SQLError> {
        self.context.resolve_function_overload_with_builtins(
            name,
            binding,
            argument_names,
            argument_types,
            explicit_variadic,
            builtins,
        )
    }

    fn resolve_scalar_subquery_type(
        &self,
        subquery: crate::SubqueryId,
        outer_schema: &crate::RowSchema,
        params: &[SQLParam],
    ) -> Result<Option<uqa_sql::ast::ColumnType>, SQLError> {
        self.context
            .resolve_scalar_subquery_type(subquery, outer_schema, params)
    }
}
