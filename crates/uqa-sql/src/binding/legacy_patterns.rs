//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Restore predecessor compiler pattern markers only at retained-definition boundaries.

use crate::{
    ast::{ColumnDef, Expr, FunctionBinding, FunctionDispatch, PatternOperator, TableCheck},
    plan::QueryPlan,
    FunctionTypeResolver, ScalarExpr,
};

type PatternCall<'a, T> = (
    &'a mut String,
    &'a mut Option<FunctionBinding>,
    &'a mut Vec<T>,
);

trait PatternNode: Sized {
    fn call(&mut self) -> Option<PatternCall<'_, Self>>;
    fn negated(&mut self) -> Option<&mut Self>;
    fn native(dispatch: FunctionDispatch, arguments: Vec<Self>) -> Self;
    fn null() -> Self;
}

macro_rules! pattern_node {
    ($node:ty) => {
        impl PatternNode for $node {
            fn call(&mut self) -> Option<PatternCall<'_, Self>> {
                match self {
                    Self::Func {
                        name,
                        binding,
                        args,
                        distinct: false,
                        order_by,
                        filter: None,
                        ..
                    } if order_by.is_empty() => Some((name, binding, args)),
                    _ => None,
                }
            }

            fn negated(&mut self) -> Option<&mut Self> {
                match self {
                    Self::Not(inner) => Some(inner),
                    _ => None,
                }
            }

            fn native(dispatch: FunctionDispatch, args: Vec<Self>) -> Self {
                let binding = FunctionBinding::dispatched(dispatch);
                Self::Func {
                    name: binding.name.clone(),
                    binding: Some(binding),
                    args,
                    order_syntax: crate::ast::FunctionCallSyntax::Ordinary,
                    distinct: false,
                    order_by: Vec::new(),
                    filter: None,
                }
            }

            fn null() -> Self {
                Self::Literal(uqa_core::Value::Null)
            }
        }
    };
}

pattern_node!(Expr);
pattern_node!(ScalarExpr);

fn restore_call<T: PatternNode>(
    expression: &mut T,
    negated: bool,
    resolver: &dyn FunctionTypeResolver,
) -> bool {
    let Some((name, binding, arguments)) = expression.call() else {
        return false;
    };
    if binding.as_ref().is_some_and(|binding| {
        !binding.builtin
            || binding.dispatch.is_some()
            || binding.object_id.is_some()
            || binding.resolution_error.is_some()
            || !binding.argument_types.is_empty()
    }) || binding.is_none() && resolver.has_untyped_function(name)
    {
        return false;
    }
    let (operator, similar) = match name.as_str() {
        "like" if (2..=3).contains(&arguments.len()) => (
            if negated {
                PatternOperator::NotLike
            } else {
                PatternOperator::Like
            },
            false,
        ),
        "ilike" if (2..=3).contains(&arguments.len()) => (
            if negated {
                PatternOperator::NotILike
            } else {
                PatternOperator::ILike
            },
            false,
        ),
        "similar_to" if (2..=3).contains(&arguments.len()) => (
            if negated {
                PatternOperator::NotRegex
            } else {
                PatternOperator::Regex
            },
            true,
        ),
        "regex_match_op" if arguments.len() == 2 => (
            if negated {
                PatternOperator::NotRegex
            } else {
                PatternOperator::Regex
            },
            false,
        ),
        "regex_imatch_op" if arguments.len() == 2 => (
            if negated {
                PatternOperator::NotRegexInsensitive
            } else {
                PatternOperator::RegexInsensitive
            },
            false,
        ),
        _ => return false,
    };
    if similar || arguments.len() == 3 {
        let helper_arguments = arguments.split_off(1);
        arguments.push(T::native(
            FunctionDispatch::PatternEscape { similar },
            helper_arguments,
        ));
    }
    let restored = FunctionBinding::dispatched(FunctionDispatch::Pattern {
        operator,
        oid: None,
    });
    name.clone_from(&restored.name);
    *binding = Some(restored);
    true
}

fn restore_node<T: PatternNode>(
    expression: &mut T,
    negative_only: bool,
    resolver: &dyn FunctionTypeResolver,
) -> bool {
    if let Some(inner) = expression.negated() {
        if restore_call(inner, true, resolver) {
            *expression = std::mem::replace(inner, T::null());
            return true;
        }
    }
    !negative_only && restore_call(expression, false, resolver)
}

/// The scalar walker visits children first. Identify legacy negative calls before replacing positive markers.
pub fn restore_query(plan: &mut QueryPlan, resolver: &dyn FunctionTypeResolver) -> bool {
    let mut changed = false;
    for negative_only in [true, false] {
        plan.rewrite_scalar_expressions(&mut |expression| {
            changed |= restore_node(expression, negative_only, resolver);
        });
    }
    changed
}

/// Keep analyzed rule constants and subquery identities while upgrading their pattern nodes.
pub fn restore_expression_plan(
    plan: &mut crate::plan::ExpressionPlan,
    resolver: &dyn FunctionTypeResolver,
) -> bool {
    let mut changed = false;
    for negative_only in [true, false] {
        crate::plan::rewrite_scalar_expression(&mut plan.scalar, &mut |expression| {
            changed |= restore_node(expression, negative_only, resolver);
        });
    }
    for subquery in &mut plan.subqueries {
        changed |= restore_query(subquery, resolver);
    }
    changed
}

/// Restore retained statement syntax before binding copies, so new escape children have matching syntax sites.
pub fn restore_statement(
    statement: &mut crate::ast::Statement,
    resolver: &dyn FunctionTypeResolver,
) -> Result<bool, crate::SQLError> {
    let mut changed = false;
    crate::catalog::stored_ast::visit_stored_statement_expressions(statement, &mut |expression| {
        changed |= restore_node(expression, false, resolver);
        Ok(())
    })?;
    Ok(changed)
}

/// Stored syntax traversal visits parents first, retaining outer negation before its child changes.
pub fn restore_expression(
    expression: &mut Expr,
    resolver: &dyn FunctionTypeResolver,
) -> Result<bool, String> {
    let mut changed = false;
    crate::catalog::stored_ast::visit_stored_expression(expression, &mut |expression| {
        changed |= restore_node(expression, false, resolver);
        Ok(())
    })
    .map_err(|error| error.to_string())?;
    Ok(changed)
}

#[derive(Default)]
pub struct SchemaPatternMigration {
    pub changed: bool,
    pub generated: bool,
}

/// The retained-resource adapter uses these flags to rebind and publish in its existing initial-open transaction.
pub fn restore_schema(
    columns: &mut [ColumnDef],
    checks: &mut [TableCheck],
    resolver: &dyn FunctionTypeResolver,
) -> Result<SchemaPatternMigration, String> {
    let mut migration = SchemaPatternMigration::default();
    for column in columns {
        for expression in [&mut column.default, &mut column.check]
            .into_iter()
            .flatten()
        {
            migration.changed |= restore_expression(expression, resolver)?;
        }
        if let Some(generated) = &mut column.generated {
            migration.generated |= restore_expression(&mut generated.expression, resolver)?;
        }
    }
    for check in checks {
        migration.changed |= restore_expression(&mut check.expr, resolver)?;
    }
    migration.changed |= migration.generated;
    Ok(migration)
}

pub(crate) fn requires_binding(binding: &FunctionBinding) -> bool {
    matches!(
        binding.dispatch,
        Some(FunctionDispatch::Pattern { oid: None, .. })
    ) || matches!(
        binding.dispatch,
        Some(FunctionDispatch::PatternEscape { .. })
    ) && binding.argument_types.is_empty()
}

#[cfg(test)]
mod tests;
