//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{
    bind_expr, BTreeMap, BinaryOp, Expr, ProjectedRuntimeRuleResolver, RuleColumnMetadata,
    RuleContext, RuleRowImage, RuleRowSide, SQLError, Value,
};

fn evaluate_rule_condition_piece<F>(
    context: RuleContext<'_>,
    expression: &Expr,
    resolver: &mut ProjectedRuntimeRuleResolver<'_, F>,
) -> Result<Value, SQLError>
where
    F: FnMut(usize, RuleRowSide, &str) -> Result<Option<Value>, SQLError>,
{
    let bound = bind_rule_condition_expression(context, expression, resolver)?;
    context.expressions.evaluate(&bound)
}

fn bind_rule_condition_expressions<F>(
    context: RuleContext<'_>,
    expressions: &[Expr],
    resolver: &mut ProjectedRuntimeRuleResolver<'_, F>,
) -> Result<Vec<Expr>, SQLError>
where
    F: FnMut(usize, RuleRowSide, &str) -> Result<Option<Value>, SQLError>,
{
    expressions
        .iter()
        .map(|expression| bind_rule_condition_expression(context, expression, resolver))
        .collect()
}

#[expect(
    clippy::too_many_lines,
    reason = "preserves action and RETURNING order"
)]
fn bind_rule_condition_expression<F>(
    context: RuleContext<'_>,
    expression: &Expr,
    resolver: &mut ProjectedRuntimeRuleResolver<'_, F>,
) -> Result<Expr, SQLError>
where
    F: FnMut(usize, RuleRowSide, &str) -> Result<Option<Value>, SQLError>,
{
    Ok(match expression {
        Expr::Case {
            base,
            when,
            else_branch,
        } => {
            let base = base
                .as_deref()
                .map(|base| evaluate_rule_condition_piece(context, base, resolver))
                .transpose()?;
            let mut selected = None;
            for (condition, result) in when {
                let condition = evaluate_rule_condition_piece(context, condition, resolver)?;
                let matches = if let Some(base) = base.as_ref() {
                    matches!(
                        uqa_sql::expr::eval_binary_values(BinaryOp::Equal, base, &condition)?,
                        Value::Bool(true)
                    )
                } else {
                    uqa_sql::expr::truthy(&condition)
                };
                if matches {
                    selected = Some(bind_rule_condition_expression(context, result, resolver)?);
                    break;
                }
            }
            if let Some(selected) = selected {
                selected
            } else if let Some(branch) = else_branch.as_deref() {
                bind_rule_condition_expression(context, branch, resolver)?
            } else {
                Expr::Literal(Value::Null)
            }
        }
        Expr::And(items) => {
            let mut saw_null = false;
            let mut result = Value::Bool(true);
            for item in items {
                let value = evaluate_rule_condition_piece(context, item, resolver)?;
                if matches!(value, Value::Null) {
                    saw_null = true;
                } else if !uqa_sql::expr::truthy(&value) {
                    result = Value::Bool(false);
                    saw_null = false;
                    break;
                }
            }
            if saw_null {
                result = Value::Null;
            }
            Expr::Literal(result)
        }
        Expr::Or(items) => {
            let mut saw_null = false;
            let mut result = Value::Bool(false);
            for item in items {
                let value = evaluate_rule_condition_piece(context, item, resolver)?;
                if matches!(value, Value::Null) {
                    saw_null = true;
                } else if uqa_sql::expr::truthy(&value) {
                    result = Value::Bool(true);
                    saw_null = false;
                    break;
                }
            }
            if saw_null {
                result = Value::Null;
            }
            Expr::Literal(result)
        }
        Expr::Func {
            name,
            binding,
            args,
            distinct,
            order_by,
            filter,
        } => Expr::Func {
            name: name.clone(),
            binding: binding.clone(),
            args: bind_rule_condition_expressions(context, args, resolver)?,
            distinct: *distinct,
            order_by: order_by
                .iter()
                .map(|order| {
                    Ok(uqa_sql::ast::OrderBy {
                        expr: bind_rule_condition_expression(context, &order.expr, resolver)?,
                        descending: order.descending,
                        nulls: order.nulls,
                    })
                })
                .collect::<Result<Vec<_>, SQLError>>()?,
            filter: filter
                .as_deref()
                .map(|filter| {
                    bind_rule_condition_expression(context, filter, resolver).map(Box::new)
                })
                .transpose()?,
        },
        Expr::Array(items) => {
            Expr::Array(bind_rule_condition_expressions(context, items, resolver)?)
        }
        Expr::Row(items) => Expr::Row(bind_rule_condition_expressions(context, items, resolver)?),
        Expr::Binary { op, lhs, rhs } => Expr::Binary {
            op: *op,
            lhs: Box::new(bind_rule_condition_expression(context, lhs, resolver)?),
            rhs: Box::new(bind_rule_condition_expression(context, rhs, resolver)?),
        },
        Expr::UnaryMinus(inner) => Expr::UnaryMinus(Box::new(bind_rule_condition_expression(
            context, inner, resolver,
        )?)),
        Expr::Not(inner) => Expr::Not(Box::new(bind_rule_condition_expression(
            context, inner, resolver,
        )?)),
        Expr::IsNull { expr, negated } => Expr::IsNull {
            expr: Box::new(bind_rule_condition_expression(context, expr, resolver)?),
            negated: *negated,
        },
        Expr::Between { expr, low, high } => Expr::Between {
            expr: Box::new(bind_rule_condition_expression(context, expr, resolver)?),
            low: Box::new(bind_rule_condition_expression(context, low, resolver)?),
            high: Box::new(bind_rule_condition_expression(context, high, resolver)?),
        },
        Expr::InList {
            expr,
            list,
            negated,
        } => Expr::InList {
            expr: Box::new(bind_rule_condition_expression(context, expr, resolver)?),
            list: bind_rule_condition_expressions(context, list, resolver)?,
            negated: *negated,
        },
        Expr::Cast { expr, ty } => Expr::Cast {
            expr: Box::new(bind_rule_condition_expression(context, expr, resolver)?),
            ty: ty.clone(),
        },
        Expr::WindowCall { .. }
        | Expr::ScalarSubquery(_)
        | Expr::Exists { .. }
        | Expr::InSubquery { .. }
        | Expr::Column(_)
        | Expr::QualifiedColumn { .. }
        | Expr::Param(_)
        | Expr::InternalColumn(_)
        | Expr::Default
        | Expr::Literal(_)
        | Expr::TypedLiteral { .. }
        | Expr::Star
        | Expr::QualifiedStar(_) => bind_expr(expression, resolver)?,
    })
}

fn materialize_rule_condition_row<F>(
    binding: &uqa_sql::catalog::events::RuleConditionBinding,
    required_columns: &std::collections::BTreeSet<String>,
    resolver: &mut ProjectedRuntimeRuleResolver<'_, F>,
) -> Result<(crate::RowSchema, crate::PhysicalRow), SQLError>
where
    F: FnMut(usize, RuleRowSide, &str) -> Result<Option<Value>, SQLError>,
{
    let mut names = Vec::with_capacity(resolver.columns.len() * 2);
    let mut identities = Vec::with_capacity(resolver.columns.len() * 2);
    let mut types = Vec::with_capacity(resolver.columns.len() * 2);
    let mut values = Vec::with_capacity(resolver.columns.len() * 2);
    let mut internal = Vec::with_capacity(resolver.columns.len() * 2);
    for (qualifier, side, relation) in [
        ("old", RuleRowSide::Old, binding.old_relation()),
        ("new", RuleRowSide::New, binding.new_relation()),
    ] {
        if relation.is_none() {
            continue;
        }
        for (name, metadata) in resolver.columns {
            if !required_columns.contains(name) {
                continue;
            }
            let slot = names.len();
            names.push(name.clone());
            identities.push(crate::ColumnIdentity::qualified(qualifier, name));
            types.push(Some(metadata.ty.clone()));
            values.push(resolver.record_field(side, name)?.value);
            let column = match side {
                RuleRowSide::Old => binding.old_column(name),
                RuleRowSide::New => binding.new_column(name),
            };
            if let Some(column) = column {
                internal.push((column, slot, Some(metadata.ty.clone())));
            }
        }
    }
    let schema = crate::RowSchema::with_identities(names, identities, types);
    Ok((
        crate::RowSchema::with_physical_internal_aliases(&schema, &internal),
        crate::PhysicalRow::from_values(values),
    ))
}

pub(super) fn rule_condition_matches<F>(
    context: RuleContext<'_>,
    rule: &uqa_sql::catalog::events::StoredRule,
    privilege_subject: &str,
    row_index: usize,
    row: &mut RuleRowImage,
    columns: &BTreeMap<String, RuleColumnMetadata>,
    project: &mut F,
) -> Result<bool, SQLError>
where
    F: FnMut(usize, RuleRowSide, &str) -> Result<Option<Value>, SQLError>,
{
    let Some(condition) = rule.definition.condition.as_ref() else {
        return Ok(true);
    };
    if let Some((plan, binding)) = rule.bound_condition_plan() {
        let mut required_columns =
            uqa_sql::semantics::rules::action_binding::rule_condition_plan_row_columns(
                plan, binding,
            );
        if uqa_sql::semantics::rules::action_binding::rule_condition_plan_references_whole_row(plan)
        {
            required_columns.extend(columns.keys().cloned());
        }
        let mut resolver = ProjectedRuntimeRuleResolver {
            row_index,
            row,
            columns,
            project,
        };
        let (schema, physical_row) =
            materialize_rule_condition_row(binding, &required_columns, &mut resolver)?;
        return Ok(uqa_sql::expr::truthy(
            &context.expressions.evaluate_stored(
                plan,
                &schema,
                &physical_row,
                privilege_subject,
            )?,
        ));
    }
    let condition = bind_rule_condition_expression(
        context,
        condition,
        &mut ProjectedRuntimeRuleResolver {
            row_index,
            row,
            columns,
            project,
        },
    )?;
    Ok(uqa_sql::expr::truthy(
        &context.expressions.evaluate(&condition)?,
    ))
}
