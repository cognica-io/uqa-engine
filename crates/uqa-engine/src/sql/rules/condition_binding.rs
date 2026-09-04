//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{
    bind_expr, eval_lowered_expression, eval_stored_expression_plan_with_row, BTreeMap, BinaryOp,
    Engine, Expr, ProjectedRuntimeRuleResolver, RuleColumnMetadata, RuleRowImage, RuleRowSide,
    SQLError, Value,
};

fn evaluate_rule_condition_piece<F>(
    engine: &Engine,
    expression: &Expr,
    resolver: &mut ProjectedRuntimeRuleResolver<'_, F>,
) -> Result<Value, SQLError>
where
    F: FnMut(usize, RuleRowSide, &str) -> Result<Option<Value>, SQLError>,
{
    let bound = bind_rule_condition_expression(engine, expression, resolver)?;
    eval_lowered_expression(engine, &bound, None, &[])
}

fn bind_rule_condition_expressions<F>(
    engine: &Engine,
    expressions: &[Expr],
    resolver: &mut ProjectedRuntimeRuleResolver<'_, F>,
) -> Result<Vec<Expr>, SQLError>
where
    F: FnMut(usize, RuleRowSide, &str) -> Result<Option<Value>, SQLError>,
{
    expressions
        .iter()
        .map(|expression| bind_rule_condition_expression(engine, expression, resolver))
        .collect()
}

#[expect(
    clippy::too_many_lines,
    reason = "preserves action and RETURNING order"
)]
fn bind_rule_condition_expression<F>(
    engine: &Engine,
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
                .map(|base| evaluate_rule_condition_piece(engine, base, resolver))
                .transpose()?;
            let mut selected = None;
            for (condition, result) in when {
                let condition = evaluate_rule_condition_piece(engine, condition, resolver)?;
                let matches = if let Some(base) = base.as_ref() {
                    matches!(
                        uqa_sql::expr::eval_binary_values(BinaryOp::Equal, base, &condition)?,
                        Value::Bool(true)
                    )
                } else {
                    uqa_sql::expr::truthy(&condition)
                };
                if matches {
                    selected = Some(bind_rule_condition_expression(engine, result, resolver)?);
                    break;
                }
            }
            if let Some(selected) = selected {
                selected
            } else if let Some(branch) = else_branch.as_deref() {
                bind_rule_condition_expression(engine, branch, resolver)?
            } else {
                Expr::Literal(Value::Null)
            }
        }
        Expr::And(items) => {
            let mut saw_null = false;
            let mut result = Value::Bool(true);
            for item in items {
                let value = evaluate_rule_condition_piece(engine, item, resolver)?;
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
                let value = evaluate_rule_condition_piece(engine, item, resolver)?;
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
            args: bind_rule_condition_expressions(engine, args, resolver)?,
            distinct: *distinct,
            order_by: order_by
                .iter()
                .map(|order| {
                    Ok(uqa_sql::ast::OrderBy {
                        expr: bind_rule_condition_expression(engine, &order.expr, resolver)?,
                        descending: order.descending,
                        nulls: order.nulls,
                    })
                })
                .collect::<Result<Vec<_>, SQLError>>()?,
            filter: filter
                .as_deref()
                .map(|filter| {
                    bind_rule_condition_expression(engine, filter, resolver).map(Box::new)
                })
                .transpose()?,
        },
        Expr::Array(items) => {
            Expr::Array(bind_rule_condition_expressions(engine, items, resolver)?)
        }
        Expr::Row(items) => Expr::Row(bind_rule_condition_expressions(engine, items, resolver)?),
        Expr::Binary { op, lhs, rhs } => Expr::Binary {
            op: *op,
            lhs: Box::new(bind_rule_condition_expression(engine, lhs, resolver)?),
            rhs: Box::new(bind_rule_condition_expression(engine, rhs, resolver)?),
        },
        Expr::UnaryMinus(inner) => Expr::UnaryMinus(Box::new(bind_rule_condition_expression(
            engine, inner, resolver,
        )?)),
        Expr::Not(inner) => Expr::Not(Box::new(bind_rule_condition_expression(
            engine, inner, resolver,
        )?)),
        Expr::IsNull { expr, negated } => Expr::IsNull {
            expr: Box::new(bind_rule_condition_expression(engine, expr, resolver)?),
            negated: *negated,
        },
        Expr::Between { expr, low, high } => Expr::Between {
            expr: Box::new(bind_rule_condition_expression(engine, expr, resolver)?),
            low: Box::new(bind_rule_condition_expression(engine, low, resolver)?),
            high: Box::new(bind_rule_condition_expression(engine, high, resolver)?),
        },
        Expr::InList {
            expr,
            list,
            negated,
        } => Expr::InList {
            expr: Box::new(bind_rule_condition_expression(engine, expr, resolver)?),
            list: bind_rule_condition_expressions(engine, list, resolver)?,
            negated: *negated,
        },
        Expr::Cast { expr, ty } => Expr::Cast {
            expr: Box::new(bind_rule_condition_expression(engine, expr, resolver)?),
            ty: ty.clone(),
        },
        Expr::WindowCall { .. }
        | Expr::ScalarSubquery(_)
        | Expr::Exists { .. }
        | Expr::InSubquery { .. } => bind_expr(expression, resolver)?,
        Expr::Column(_)
        | Expr::QualifiedColumn { .. }
        | Expr::Param(_)
        | Expr::InternalColumn(_)
        | Expr::Default
        | Expr::Literal(_)
        | Expr::Star
        | Expr::QualifiedStar(_) => bind_expr(expression, resolver)?,
    })
}

fn materialize_rule_condition_row<F>(
    binding: &crate::engine_events::RuleConditionBinding,
    required_columns: &std::collections::BTreeSet<String>,
    resolver: &mut ProjectedRuntimeRuleResolver<'_, F>,
) -> Result<(uqa_execution::RowSchema, uqa_execution::PhysicalRow), SQLError>
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
            identities.push(uqa_execution::ColumnIdentity::qualified(qualifier, name));
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
    let schema = uqa_execution::RowSchema::with_identities(names, identities, types);
    Ok((
        uqa_execution::RowSchema::with_physical_internal_aliases(&schema, &internal),
        uqa_execution::PhysicalRow::from_values(values),
    ))
}

pub(super) fn rule_condition_matches<F>(
    engine: &Engine,
    rule: &crate::engine_events::StoredRule,
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
            crate::engine_events::rule_condition_plan_row_columns(plan, binding);
        if crate::engine_events::rule_condition_plan_references_whole_row(plan) {
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
            &eval_stored_expression_plan_with_row(
                engine,
                plan,
                &schema,
                &physical_row,
                &[],
                Some(privilege_subject),
            )?,
        ));
    }
    let condition = bind_rule_condition_expression(
        engine,
        condition,
        &mut ProjectedRuntimeRuleResolver {
            row_index,
            row,
            columns,
            project,
        },
    )?;
    Ok(uqa_sql::expr::truthy(&eval_lowered_expression(
        engine,
        &condition,
        None,
        &[],
    )?))
}
