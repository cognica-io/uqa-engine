//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Set-returning and aggregate dependency expression rewrites.

use crate::plan::{ProjectionPlan, QueryBlockPlan};
use crate::{FunctionTypeResolver, RowSchema, ScalarExpr};
use crate::{SQLError, SQLParam};
use uqa_core::Value;

use super::validation::{
    expression_may_return_set, function_may_return_set, resolve_set_function_binding,
};
use super::{
    AggregateOutputProjectionPlan, GroupSetProjectionPlan, ProjectionTarget, SetFunctionCall,
    SetFunctionCatalog,
};
use crate::semantics::aggregates::{exprs_match, is_aggregate};
use crate::semantics::projection_columns;

pub fn rewrite_set_calls(
    engine: &dyn SetFunctionCatalog,
    resolver: &dyn FunctionTypeResolver,
    mut expression: ScalarExpr,
    calls: &mut Vec<SetFunctionCall>,
    call_relation: crate::ast::InternalRelationId,
    schema: &RowSchema,
    params: &[SQLParam],
) -> Result<ScalarExpr, SQLError> {
    SetCallRewrite {
        engine,
        resolver,
        calls,
        call_relation,
        schema,
        params,
    }
    .rewrite(&mut expression)?;
    Ok(expression)
}

struct SetCallRewrite<'a> {
    engine: &'a dyn SetFunctionCatalog,
    resolver: &'a dyn FunctionTypeResolver,
    calls: &'a mut Vec<SetFunctionCall>,
    call_relation: crate::ast::InternalRelationId,
    schema: &'a RowSchema,
    params: &'a [SQLParam],
}

impl SetCallRewrite<'_> {
    /// Return the deepest set-call level in this subtree. A parent can assign its level without scanning the accumulated call list again.
    fn rewrite(&mut self, expression: &mut ScalarExpr) -> Result<Option<usize>, SQLError> {
        let mut deepest = None;
        for child in expression.children_mut(crate::ir::children::Scope::Complete) {
            deepest = deepest.max(self.rewrite(child)?);
        }
        let ScalarExpr::Func {
            name,
            binding,
            args,
            ..
        } = expression
        else {
            return Ok(deepest);
        };
        if !function_may_return_set(
            self.engine,
            self.resolver,
            name,
            binding.as_ref(),
            args,
            self.schema,
            self.params,
        )? {
            return Ok(deepest);
        }
        let binding = resolve_set_function_binding(
            self.engine,
            self.resolver,
            name,
            binding.as_ref(),
            args,
            self.schema,
            self.params,
        )?
        .or_else(|| binding.take());
        let level = deepest.map_or(0, |level| level + 1);
        let placeholder = self.call_relation.column(self.calls.len());
        self.calls.push(SetFunctionCall {
            placeholder,
            name: std::mem::take(name),
            binding,
            args: std::mem::take(args),
            level,
        });
        *expression = ScalarExpr::InternalColumn(placeholder);
        Ok(Some(level))
    }
}

fn replace_group_set_expression(
    expression: &mut ScalarExpr,
    mappings: &[(ScalarExpr, crate::ast::InternalColumnRef)],
) {
    expression.visit_mut(&mut |node| {
        if let Some((_, column)) = mappings.iter().find(|(group, _)| exprs_match(node, group)) {
            *node = ScalarExpr::InternalColumn(*column);
        }
    });
}

pub fn prepare_group_set_projection(
    engine: &dyn SetFunctionCatalog,
    resolver: &dyn FunctionTypeResolver,
    statement: &QueryBlockPlan,
    schema: &RowSchema,
    params: &[SQLParam],
) -> Result<Option<GroupSetProjectionPlan>, SQLError> {
    let mut groups = Vec::new();
    for expression in statement
        .group_by
        .iter()
        .chain(statement.grouping_sets.iter().flatten())
    {
        if expression_may_return_set(engine, resolver, expression, schema, params)?
            && !groups
                .iter()
                .any(|existing| exprs_match(existing, expression))
        {
            groups.push(expression.clone());
        }
    }
    if groups.is_empty() {
        return Ok(None);
    }

    let relation = crate::ast::InternalRelationId::allocate();
    let mappings = groups
        .iter()
        .enumerate()
        .map(|(index, expression)| (expression.clone(), relation.column(index)))
        .collect::<Vec<_>>();
    let projections = mappings
        .iter()
        .map(|(expression, column)| (ProjectionTarget::Internal(*column), expression.clone()))
        .collect();
    let mut rewritten = statement.clone();
    let projection_labels = projection_columns(&rewritten.projections);
    for expression in &mut rewritten.group_by {
        replace_group_set_expression(expression, &mappings);
    }
    for set in &mut rewritten.grouping_sets {
        for expression in set {
            replace_group_set_expression(expression, &mappings);
        }
    }
    for (projection, label) in rewritten.projections.iter_mut().zip(projection_labels) {
        if projection.alias.is_none() {
            projection.alias = Some(label);
        }
        replace_group_set_expression(&mut projection.expr, &mappings);
    }
    if let Some(having) = &mut rewritten.having {
        replace_group_set_expression(having, &mappings);
    }
    for order in &mut rewritten.order_by {
        replace_group_set_expression(&mut order.expr, &mappings);
    }
    for expression in &mut rewritten.distinct_on {
        replace_group_set_expression(expression, &mappings);
    }
    Ok(Some(GroupSetProjectionPlan {
        statement: rewritten,
        projections,
    }))
}

fn rewrite_aggregate_dependencies(
    engine: &dyn SetFunctionCatalog,
    group_by: &[&ScalarExpr],
    expression: &mut ScalarExpr,
    dependencies: &mut Vec<ProjectionPlan>,
) {
    if is_aggregate(engine, expression)
        || group_by.iter().any(|group| exprs_match(expression, group))
        || matches!(
            expression,
            ScalarExpr::Column(_)
                | ScalarExpr::QualifiedColumn { .. }
                | ScalarExpr::Position(_)
                | ScalarExpr::InternalColumn(_)
        )
    {
        let position = ScalarExpr::Position(dependencies.len());
        dependencies.push(ProjectionPlan {
            expr: std::mem::replace(expression, position),
            alias: None,
        });
    } else {
        for child in expression.children_mut(crate::ir::children::Scope::AggregateDependencies) {
            rewrite_aggregate_dependencies(engine, group_by, child, dependencies);
        }
    }
}

pub fn prepare_aggregate_output_projection(
    engine: &dyn SetFunctionCatalog,
    statement: &QueryBlockPlan,
    internal_targets: &[(usize, crate::ast::InternalColumnRef)],
) -> AggregateOutputProjectionPlan {
    let labels = projection_columns(&statement.projections);
    // A grouping-set expression remains one aggregate dependency. Splitting it into its input columns would replace those ungrouped inputs with NULL before evaluating the final projection.
    let group_by = statement
        .group_by
        .iter()
        .chain(statement.grouping_sets.iter().flatten())
        .collect::<Vec<_>>();
    let mut dependencies = Vec::new();
    let mut aggregate_statement = statement.clone();
    let projections = std::mem::take(&mut aggregate_statement.projections)
        .into_iter()
        .enumerate()
        .zip(labels)
        .map(|((position, mut projection), label)| {
            let target = internal_targets
                .iter()
                .find(|(target_position, _)| *target_position == position)
                .map_or_else(
                    || ProjectionTarget::Column(label),
                    |(_, column)| ProjectionTarget::Internal(*column),
                );
            rewrite_aggregate_dependencies(
                engine,
                &group_by,
                &mut projection.expr,
                &mut dependencies,
            );
            (target, projection.expr)
        })
        .collect();
    if dependencies.is_empty() {
        dependencies.push(ProjectionPlan {
            expr: ScalarExpr::Literal(Value::Int(1)),
            alias: None,
        });
    }
    aggregate_statement.projections = dependencies;
    AggregateOutputProjectionPlan {
        statement: aggregate_statement,
        projections,
    }
}
