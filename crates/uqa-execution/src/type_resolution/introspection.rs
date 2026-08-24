//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use uqa_core::Value;
use uqa_sql::ast::ColumnType;
use uqa_sql::SQLParam;

use crate::{RowSchema, ScalarExpr};

use super::common::{base_type, common_context_expression_type, merge_optional_types};
use super::operators::unary_minus_result_type;
use super::{array_transform, containment, fixed_builtin, scalar_type_inner, FunctionTypeResolver};

/// Bind polymorphic type-introspection calls and common-type coercions while the input schema still carries declared SQL types. Runtime values deliberately do not encode integer widths, varchar identity, or float widths, and selector expressions must return the common SQL type rather than the storage type of the branch selected at runtime.
pub fn bind_type_introspection(
    expression: ScalarExpr,
    schema: &RowSchema,
    params: &[SQLParam],
) -> ScalarExpr {
    bind_type_introspection_inner(expression, schema, params, None)
}

/// Bind type-introspection calls with access to catalog-backed function and aggregate overloads.
pub fn bind_type_introspection_with_resolver(
    expression: ScalarExpr,
    schema: &RowSchema,
    params: &[SQLParam],
    resolver: &dyn FunctionTypeResolver,
) -> ScalarExpr {
    bind_type_introspection_inner(expression, schema, params, Some(resolver))
}

fn bind_type_introspection_inner(
    expression: ScalarExpr,
    schema: &RowSchema,
    params: &[SQLParam],
    resolver: Option<&dyn FunctionTypeResolver>,
) -> ScalarExpr {
    if !requires_type_introspection_binding(&expression) {
        return expression;
    }
    match expression {
        ScalarExpr::Func {
            name,
            mut binding,
            mut args,
            distinct,
            mut order_by,
            mut filter,
        } => {
            for argument in &mut args {
                bind_type_introspection_in_place(argument, schema, params, resolver);
            }
            for order in &mut order_by {
                bind_type_introspection_in_place(&mut order.expr, schema, params, resolver);
            }
            if let Some(filter) = filter.as_deref_mut() {
                bind_type_introspection_in_place(filter, schema, params, resolver);
            }
            if containment::is_operator(&name) {
                containment::bind_unknown_arguments(&mut args, schema, params, resolver);
            }
            if is_common_type_function(&name) {
                bind_common_type_expressions(&mut args, schema, params, resolver);
            }
            let name =
                array_transform::bind_call(name, &mut binding, &mut args, schema, params, resolver);
            let name =
                fixed_builtin::bind_call(name, &mut binding, &mut args, schema, params, resolver);
            if is_pg_typeof(&name) && args.len() == 1 {
                let name = scalar_type_inner(&args[0], schema, params, resolver)
                    .ok()
                    .flatten()
                    .map_or_else(|| "unknown".to_string(), |ty| ty.regtype_name());
                return ScalarExpr::Cast {
                    expr: Box::new(ScalarExpr::Literal(Value::Str(name))),
                    ty: "regtype".into(),
                };
            }
            ScalarExpr::Func {
                name,
                binding,
                args,
                distinct,
                order_by,
                filter,
            }
        }
        ScalarExpr::Array(mut items) => {
            bind_type_introspection_items(&mut items, schema, params, resolver);
            bind_common_type_expressions(&mut items, schema, params, resolver);
            ScalarExpr::Array(items)
        }
        ScalarExpr::Row(mut items) => {
            bind_type_introspection_items(&mut items, schema, params, resolver);
            ScalarExpr::Row(items)
        }
        ScalarExpr::Binary {
            op,
            mut lhs,
            mut rhs,
        } => {
            bind_type_introspection_in_place(lhs.as_mut(), schema, params, resolver);
            bind_type_introspection_in_place(rhs.as_mut(), schema, params, resolver);
            ScalarExpr::Binary { op, lhs, rhs }
        }
        ScalarExpr::UnaryMinus(mut expr) => {
            let source_type = scalar_type_inner(&expr, schema, params, resolver)
                .ok()
                .flatten()
                .and_then(|ty| unary_minus_result_type(&ty).ok());
            bind_type_introspection_in_place(expr.as_mut(), schema, params, resolver);
            if let Some(source_type) = source_type {
                wrap_in_declared_cast(expr.as_mut(), &source_type);
            }
            ScalarExpr::UnaryMinus(expr)
        }
        ScalarExpr::Not(mut inner) => {
            bind_type_introspection_in_place(inner.as_mut(), schema, params, resolver);
            ScalarExpr::Not(inner)
        }
        ScalarExpr::And(mut items) => {
            bind_type_introspection_items(&mut items, schema, params, resolver);
            ScalarExpr::And(items)
        }
        ScalarExpr::Or(mut items) => {
            bind_type_introspection_items(&mut items, schema, params, resolver);
            ScalarExpr::Or(items)
        }
        ScalarExpr::IsNull { mut expr, negated } => {
            bind_type_introspection_in_place(expr.as_mut(), schema, params, resolver);
            ScalarExpr::IsNull { expr, negated }
        }
        ScalarExpr::Between {
            mut expr,
            mut low,
            mut high,
        } => {
            bind_type_introspection_in_place(expr.as_mut(), schema, params, resolver);
            bind_type_introspection_in_place(low.as_mut(), schema, params, resolver);
            bind_type_introspection_in_place(high.as_mut(), schema, params, resolver);
            ScalarExpr::Between { expr, low, high }
        }
        ScalarExpr::InList {
            mut expr,
            mut list,
            negated,
        } => {
            bind_type_introspection_in_place(expr.as_mut(), schema, params, resolver);
            bind_type_introspection_items(&mut list, schema, params, resolver);
            ScalarExpr::InList {
                expr,
                list,
                negated,
            }
        }
        ScalarExpr::WindowCall {
            name,
            mut args,
            mut spec,
        } => {
            bind_type_introspection_items(&mut args, schema, params, resolver);
            bind_type_introspection_items(&mut spec.partition_by, schema, params, resolver);
            for order in &mut spec.order_by {
                bind_type_introspection_in_place(&mut order.expr, schema, params, resolver);
            }
            if let Some(frame) = spec.frame.as_mut() {
                bind_frame_bound(&mut frame.start, schema, params, resolver);
                bind_frame_bound(&mut frame.end, schema, params, resolver);
            }
            ScalarExpr::WindowCall { name, args, spec }
        }
        ScalarExpr::Case {
            mut base,
            mut when,
            mut else_branch,
        } => {
            if let Some(base) = base.as_deref_mut() {
                bind_type_introspection_in_place(base, schema, params, resolver);
            }
            for (condition, result) in &mut when {
                bind_type_introspection_in_place(condition, schema, params, resolver);
                bind_type_introspection_in_place(result, schema, params, resolver);
            }
            if let Some(else_branch) = else_branch.as_deref_mut() {
                bind_type_introspection_in_place(else_branch, schema, params, resolver);
            }
            if base.is_some() {
                let comparison_type = common_expression_type(
                    base.iter()
                        .map(Box::as_ref)
                        .chain(when.iter().map(|(condition, _)| condition)),
                    schema,
                    params,
                    resolver,
                );
                if let Some(comparison_type) = comparison_type {
                    if let Some(base) = base.as_deref_mut() {
                        bind_common_type_cast(base, &comparison_type, schema, params, resolver);
                    }
                    for (condition, _) in &mut when {
                        bind_common_type_cast(
                            condition,
                            &comparison_type,
                            schema,
                            params,
                            resolver,
                        );
                    }
                }
            }
            let result_type = common_expression_type(
                when.iter()
                    .map(|(_, result)| result)
                    .chain(else_branch.iter().map(Box::as_ref)),
                schema,
                params,
                resolver,
            );
            if let Some(result_type) = result_type {
                for (_, result) in &mut when {
                    bind_common_type_cast(result, &result_type, schema, params, resolver);
                }
                if let Some(else_branch) = else_branch.as_deref_mut() {
                    bind_common_type_cast(else_branch, &result_type, schema, params, resolver);
                }
            }
            ScalarExpr::Case {
                base,
                when,
                else_branch,
            }
        }
        ScalarExpr::Cast { mut expr, ty } => {
            let source_type = cast_requires_declared_source(&ty)
                .then(|| {
                    scalar_type_inner(&expr, schema, params, resolver)
                        .ok()
                        .flatten()
                })
                .flatten()
                .and_then(|source_type| declared_source_wrapper(&ty, source_type));
            bind_type_introspection_in_place(expr.as_mut(), schema, params, resolver);
            if let Some(source_type) = source_type {
                wrap_in_declared_cast(expr.as_mut(), &source_type);
            }
            ScalarExpr::Cast { expr, ty }
        }
        ScalarExpr::InSubquery {
            mut expr,
            subquery,
            negated,
        } => {
            bind_type_introspection_in_place(expr.as_mut(), schema, params, resolver);
            ScalarExpr::InSubquery {
                expr,
                subquery,
                negated,
            }
        }
        other => other,
    }
}

fn requires_type_introspection_binding(expression: &ScalarExpr) -> bool {
    match expression {
        ScalarExpr::Func {
            name,
            args,
            order_by,
            filter,
            ..
        } => {
            is_pg_typeof(name)
                || is_common_type_function(name)
                || fixed_builtin::is_function(name)
                || array_transform::is_function(name)
                || containment::is_operator(name)
                || args.iter().any(requires_type_introspection_binding)
                || order_by
                    .iter()
                    .any(|order| requires_type_introspection_binding(&order.expr))
                || filter
                    .as_deref()
                    .is_some_and(requires_type_introspection_binding)
        }
        ScalarExpr::Array(_) | ScalarExpr::Case { .. } | ScalarExpr::UnaryMinus(_) => true,
        ScalarExpr::Row(items) | ScalarExpr::And(items) | ScalarExpr::Or(items) => {
            items.iter().any(requires_type_introspection_binding)
        }
        ScalarExpr::Binary { lhs, rhs, .. } => {
            requires_type_introspection_binding(lhs) || requires_type_introspection_binding(rhs)
        }
        ScalarExpr::Not(expression)
        | ScalarExpr::IsNull {
            expr: expression, ..
        } => requires_type_introspection_binding(expression),
        ScalarExpr::Between { expr, low, high } => {
            requires_type_introspection_binding(expr)
                || requires_type_introspection_binding(low)
                || requires_type_introspection_binding(high)
        }
        ScalarExpr::InList { expr, list, .. } => {
            requires_type_introspection_binding(expr)
                || list.iter().any(requires_type_introspection_binding)
        }
        ScalarExpr::WindowCall { args, spec, .. } => {
            args.iter().any(requires_type_introspection_binding)
                || spec
                    .partition_by
                    .iter()
                    .any(requires_type_introspection_binding)
                || spec
                    .order_by
                    .iter()
                    .any(|order| requires_type_introspection_binding(&order.expr))
                || spec.frame.as_ref().is_some_and(|frame| {
                    frame_bound_requires_type_introspection_binding(&frame.start)
                        || frame_bound_requires_type_introspection_binding(&frame.end)
                })
        }
        ScalarExpr::Cast { expr, ty } => {
            cast_requires_declared_source(ty) || requires_type_introspection_binding(expr)
        }
        ScalarExpr::InSubquery { expr, .. } => requires_type_introspection_binding(expr),
        ScalarExpr::Star
        | ScalarExpr::QualifiedStar(_)
        | ScalarExpr::Default
        | ScalarExpr::Column(_)
        | ScalarExpr::Position(_)
        | ScalarExpr::QualifiedColumn { .. }
        | ScalarExpr::Literal(_)
        | ScalarExpr::Param(_)
        | ScalarExpr::ScalarSubquery(_)
        | ScalarExpr::Exists { .. } => false,
    }
}

fn frame_bound_requires_type_introspection_binding(bound: &crate::ScalarFrameBound) -> bool {
    match bound {
        crate::ScalarFrameBound::Preceding(expression)
        | crate::ScalarFrameBound::Following(expression) => {
            requires_type_introspection_binding(expression)
        }
        crate::ScalarFrameBound::UnboundedPreceding
        | crate::ScalarFrameBound::UnboundedFollowing
        | crate::ScalarFrameBound::CurrentRow => false,
    }
}

fn is_pg_typeof(name: &str) -> bool {
    name.eq_ignore_ascii_case("pg_typeof") || name.eq_ignore_ascii_case("pg_catalog.pg_typeof")
}

fn is_common_type_function(name: &str) -> bool {
    ["coalesce", "greatest", "least"]
        .iter()
        .any(|candidate| name.eq_ignore_ascii_case(candidate))
}

fn bind_common_type_expressions(
    expressions: &mut [ScalarExpr],
    schema: &RowSchema,
    params: &[SQLParam],
    resolver: Option<&dyn FunctionTypeResolver>,
) {
    let Some(target) = common_expression_type(expressions.iter(), schema, params, resolver) else {
        return;
    };
    for expression in expressions {
        bind_common_type_cast(expression, &target, schema, params, resolver);
    }
}

fn common_expression_type<'a>(
    expressions: impl IntoIterator<Item = &'a ScalarExpr>,
    schema: &RowSchema,
    params: &[SQLParam],
    resolver: Option<&dyn FunctionTypeResolver>,
) -> Option<ColumnType> {
    let mut common = None;
    let mut saw_expression = false;
    for expression in expressions {
        saw_expression = true;
        let expression_type =
            common_context_expression_type(expression, schema, params, resolver).ok()?;
        common = merge_optional_types(common, expression_type).ok()?;
    }
    saw_expression.then(|| common.unwrap_or(ColumnType::Text))
}

fn bind_common_type_cast(
    expression: &mut ScalarExpr,
    target: &ColumnType,
    schema: &RowSchema,
    params: &[SQLParam],
    resolver: Option<&dyn FunctionTypeResolver>,
) {
    let target = base_type(target);
    let source = common_context_expression_type(expression, schema, params, resolver)
        .ok()
        .flatten();
    if source
        .as_ref()
        .is_some_and(|source| base_type(source) == target)
    {
        return;
    }
    let inner = std::mem::replace(expression, ScalarExpr::Literal(Value::Null));
    *expression = ScalarExpr::Cast {
        expr: Box::new(inner),
        ty: target.sql_name(),
    };
}

fn wrap_in_declared_cast(expression: &mut ScalarExpr, source_type: &ColumnType) {
    let source_name = source_type.sql_name();
    if matches!(expression, ScalarExpr::Cast { ty, .. } if ty.eq_ignore_ascii_case(&source_name)) {
        return;
    }
    let inner = std::mem::replace(expression, ScalarExpr::Literal(Value::Null));
    *expression = ScalarExpr::Cast {
        expr: Box::new(inner),
        ty: source_name,
    };
}

fn bind_type_introspection_items(
    expressions: &mut [ScalarExpr],
    schema: &RowSchema,
    params: &[SQLParam],
    resolver: Option<&dyn FunctionTypeResolver>,
) {
    for expression in expressions {
        bind_type_introspection_in_place(expression, schema, params, resolver);
    }
}

fn bind_type_introspection_in_place(
    expression: &mut ScalarExpr,
    schema: &RowSchema,
    params: &[SQLParam],
    resolver: Option<&dyn FunctionTypeResolver>,
) {
    let owned = std::mem::replace(expression, ScalarExpr::Literal(Value::Null));
    *expression = bind_type_introspection_inner(owned, schema, params, resolver);
}

fn cast_requires_declared_source(target: &str) -> bool {
    let mut target = target.trim().to_ascii_lowercase();
    while let Some(element) = target.strip_suffix("[]") {
        target = element.trim_end().to_string();
    }
    matches!(
        target.as_str(),
        "bytea"
            | "pg_catalog.bytea"
            | "oid"
            | "pg_catalog.oid"
            | "xid"
            | "pg_catalog.xid"
            | "text"
            | "pg_catalog.text"
            | "int2vector"
            | "pg_catalog.int2vector"
            | "oidvector"
            | "pg_catalog.oidvector"
    )
}

fn declared_source_wrapper(target: &str, source_type: ColumnType) -> Option<ColumnType> {
    let target = target.trim().to_ascii_lowercase();
    if matches!(target.as_str(), "text" | "pg_catalog.text") {
        return match base_type(&source_type) {
            source @ (ColumnType::Int2Vector | ColumnType::OidVector) => Some(source.clone()),
            _ => None,
        };
    }
    Some(source_type)
}

fn bind_frame_bound(
    bound: &mut crate::ScalarFrameBound,
    schema: &RowSchema,
    params: &[SQLParam],
    resolver: Option<&dyn FunctionTypeResolver>,
) {
    match bound {
        crate::ScalarFrameBound::Preceding(expression)
        | crate::ScalarFrameBound::Following(expression) => {
            bind_type_introspection_in_place(expression.as_mut(), schema, params, resolver);
        }
        crate::ScalarFrameBound::UnboundedPreceding
        | crate::ScalarFrameBound::UnboundedFollowing
        | crate::ScalarFrameBound::CurrentRow => {}
    }
}
