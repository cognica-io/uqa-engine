//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Aggregate expression replacement, input extraction, and observation.

use super::{
    eval_scalar, exprs_match, is_aggregate, AggregateAccumulator, AggregateAccumulatorTemplate,
    Engine, SQLError, ScalarEvalContext, ScalarExpr, ScalarOrder, Value,
};

const AGGREGATE_SLOT_PREFIX: &str = "\0uqa.aggregate.";

pub(in crate::sql) fn compile_projection_aggregate_slots(
    engine: &Engine,
    expr: &ScalarExpr,
    cursor: &mut usize,
) -> Result<ScalarExpr, SQLError> {
    rewrite_aggregates(engine, expr, &mut |_| {
        let slot = aggregate_slot(*cursor);
        *cursor += 1;
        Ok(slot)
    })
}

pub(in crate::sql) fn compile_having_aggregate_slots(
    engine: &Engine,
    expr: &ScalarExpr,
    aggregate_targets: &[ScalarExpr],
) -> Result<ScalarExpr, SQLError> {
    rewrite_aggregates(engine, expr, &mut |aggregate| {
        aggregate_targets
            .iter()
            .position(|target| exprs_match(target, aggregate))
            .map(aggregate_slot)
            .ok_or_else(|| {
                SQLError::Unsupported(
                    "HAVING references an aggregate that is not in the aggregate plan".into(),
                )
            })
    })
}

pub(in crate::sql) fn aggregate_slot_index(column: &str) -> Option<usize> {
    column.strip_prefix(AGGREGATE_SLOT_PREFIX)?.parse().ok()
}

fn aggregate_slot(index: usize) -> ScalarExpr {
    ScalarExpr::Column(format!("{AGGREGATE_SLOT_PREFIX}{index}"))
}

fn rewrite_aggregates(
    engine: &Engine,
    expr: &ScalarExpr,
    replace: &mut impl FnMut(&ScalarExpr) -> Result<ScalarExpr, SQLError>,
) -> Result<ScalarExpr, SQLError> {
    if is_aggregate(engine, expr) {
        return replace(expr);
    }
    match expr {
        ScalarExpr::Func {
            name,
            binding,
            args,
            distinct,
            order_by,
            filter,
        } => Ok(ScalarExpr::Func {
            name: name.clone(),
            binding: binding.clone(),
            args: args
                .iter()
                .map(|arg| rewrite_aggregates(engine, arg, replace))
                .collect::<Result<Vec<_>, _>>()?,
            distinct: *distinct,
            order_by: order_by.clone(),
            filter: filter
                .as_deref()
                .map(|filter| rewrite_aggregates(engine, filter, replace).map(Box::new))
                .transpose()?,
        }),
        ScalarExpr::Array(items) => Ok(ScalarExpr::Array(
            items
                .iter()
                .map(|item| rewrite_aggregates(engine, item, replace))
                .collect::<Result<Vec<_>, _>>()?,
        )),
        ScalarExpr::Binary { op, lhs, rhs } => Ok(ScalarExpr::Binary {
            op: *op,
            lhs: Box::new(rewrite_aggregates(engine, lhs, replace)?),
            rhs: Box::new(rewrite_aggregates(engine, rhs, replace)?),
        }),
        ScalarExpr::Not(inner) => Ok(ScalarExpr::Not(Box::new(rewrite_aggregates(
            engine, inner, replace,
        )?))),
        ScalarExpr::And(parts) => Ok(ScalarExpr::And(
            parts
                .iter()
                .map(|part| rewrite_aggregates(engine, part, replace))
                .collect::<Result<Vec<_>, _>>()?,
        )),
        ScalarExpr::Or(parts) => Ok(ScalarExpr::Or(
            parts
                .iter()
                .map(|part| rewrite_aggregates(engine, part, replace))
                .collect::<Result<Vec<_>, _>>()?,
        )),
        ScalarExpr::IsNull { expr, negated } => Ok(ScalarExpr::IsNull {
            expr: Box::new(rewrite_aggregates(engine, expr, replace)?),
            negated: *negated,
        }),
        ScalarExpr::Between { expr, low, high } => Ok(ScalarExpr::Between {
            expr: Box::new(rewrite_aggregates(engine, expr, replace)?),
            low: Box::new(rewrite_aggregates(engine, low, replace)?),
            high: Box::new(rewrite_aggregates(engine, high, replace)?),
        }),
        ScalarExpr::InList {
            expr,
            list,
            negated,
        } => Ok(ScalarExpr::InList {
            expr: Box::new(rewrite_aggregates(engine, expr, replace)?),
            list: list
                .iter()
                .map(|item| rewrite_aggregates(engine, item, replace))
                .collect::<Result<Vec<_>, _>>()?,
            negated: *negated,
        }),
        ScalarExpr::Case {
            base,
            when,
            else_branch,
        } => Ok(ScalarExpr::Case {
            base: base
                .as_deref()
                .map(|base| rewrite_aggregates(engine, base, replace).map(Box::new))
                .transpose()?,
            when: when
                .iter()
                .map(|(condition, result)| {
                    Ok((
                        rewrite_aggregates(engine, condition, replace)?,
                        rewrite_aggregates(engine, result, replace)?,
                    ))
                })
                .collect::<Result<Vec<_>, SQLError>>()?,
            else_branch: else_branch
                .as_deref()
                .map(|branch| rewrite_aggregates(engine, branch, replace).map(Box::new))
                .transpose()?,
        }),
        ScalarExpr::Cast { expr, ty } => Ok(ScalarExpr::Cast {
            expr: Box::new(rewrite_aggregates(engine, expr, replace)?),
            ty: ty.clone(),
        }),
        ScalarExpr::InSubquery {
            expr,
            subquery,
            negated,
        } => Ok(ScalarExpr::InSubquery {
            expr: Box::new(rewrite_aggregates(engine, expr, replace)?),
            subquery: *subquery,
            negated: *negated,
        }),
        other => Ok(other.clone()),
    }
}

pub(in crate::sql) fn aggregate_input_value(
    name: &str,
    args: &[ScalarExpr],
    order_by: &[ScalarOrder],
    ctx: &ScalarEvalContext<'_>,
) -> Result<Value, SQLError> {
    if name.eq_ignore_ascii_case("count") && (args.is_empty() || matches!(args, [ScalarExpr::Star]))
    {
        return Ok(Value::Int(1));
    }
    // Ordered-set aggregates: the percentile / mode fraction is a
    // direct positional argument; the value to fold comes from
    // `WITHIN GROUP (ORDER BY ...)` which the compiler parks in
    // `order_by[0]`.
    if is_ordered_set_aggregate(name) {
        return order_by
            .first()
            .map(|ob| eval_scalar(&ob.expr, ctx))
            .transpose()
            .map(|v| v.unwrap_or(Value::Null));
    }
    if is_json_object_aggregate(name) {
        return match args {
            [key_expr, value_expr] => {
                let key = eval_scalar(key_expr, ctx)?;
                if matches!(key, Value::Null) {
                    return Ok(Value::Null);
                }
                let value = eval_scalar(value_expr, ctx)?;
                Ok(Value::List(vec![key, value]))
            }
            _ => Err(SQLError::TypeMismatch(format!(
                "{name} requires 2 arguments"
            ))),
        };
    }
    if is_json_array_aggregate(name) {
        return match args {
            [arg] => eval_scalar(arg, ctx),
            _ => Err(SQLError::TypeMismatch(format!(
                "{name} requires 1 argument"
            ))),
        };
    }
    let arg = args
        .first()
        .ok_or_else(|| SQLError::Internal("aggregate missing arg".into()))?;
    eval_scalar(arg, ctx)
}

pub(in crate::sql) fn aggregate_input_values(
    args: &[ScalarExpr],
    ctx: &ScalarEvalContext<'_>,
) -> Result<Vec<Value>, SQLError> {
    args.iter()
        .map(|arg| match arg {
            ScalarExpr::Star => Ok(Value::Int(1)),
            other => eval_scalar(other, ctx),
        })
        .collect()
}

pub(in crate::sql) fn new_aggregate_accumulators_with_budget(
    engine: &Engine,
    aggregate_targets: &[ScalarExpr],
    budget_bytes: usize,
) -> Result<Vec<AggregateAccumulator>, SQLError> {
    Ok(instantiate_aggregate_accumulators(
        &aggregate_accumulator_templates(engine, aggregate_targets),
        budget_bytes,
    ))
}

pub(in crate::sql) fn aggregate_accumulator_templates(
    engine: &Engine,
    aggregate_targets: &[ScalarExpr],
) -> Vec<AggregateAccumulatorTemplate> {
    aggregate_targets
        .iter()
        .map(|expression| match expression {
            ScalarExpr::Func { name, .. } => {
                engine.registered_aggregate_function(name).map_or_else(
                    || AggregateAccumulatorTemplate::builtin(name),
                    AggregateAccumulatorTemplate::registered,
                )
            }
            _ => AggregateAccumulatorTemplate::generic(),
        })
        .collect()
}

pub(in crate::sql) fn instantiate_aggregate_accumulators(
    templates: &[AggregateAccumulatorTemplate],
    budget_bytes: usize,
) -> Vec<AggregateAccumulator> {
    templates
        .iter()
        .map(|template| template.instantiate(budget_bytes))
        .collect()
}

pub(in crate::sql) fn observe_aggregate(
    acc: &mut AggregateAccumulator,
    name: &str,
    args: &[ScalarExpr],
    distinct: bool,
    order_by: &[ScalarOrder],
    ctx: &ScalarEvalContext<'_>,
) -> Result<(), SQLError> {
    if acc.registered.is_some() {
        let values = aggregate_input_values(args, ctx)?;
        if distinct && !acc.distinct.insert(&Value::List(values.clone()))? {
            return Ok(());
        }
        let mut sort_keys: Vec<(Value, bool)> = Vec::with_capacity(order_by.len());
        for ob in order_by {
            let v = eval_scalar(&ob.expr, ctx)?;
            sort_keys.push((v, ob.descending));
        }
        acc.observe_registered(values, sort_keys)?;
        return Ok(());
    }

    let value = aggregate_input_value(name, args, order_by, ctx)?;
    observe_builtin_aggregate_value(acc, name, &value, distinct, order_by, ctx)
}

pub(in crate::sql) fn observe_builtin_aggregate_value(
    acc: &mut AggregateAccumulator,
    name: &str,
    value: &Value,
    distinct: bool,
    order_by: &[ScalarOrder],
    ctx: &ScalarEvalContext<'_>,
) -> Result<(), SQLError> {
    let preserves_null_inputs = is_json_array_aggregate(name);
    if distinct
        && (preserves_null_inputs || !matches!(value, Value::Null))
        && !acc.distinct.insert(value)?
    {
        return Ok(());
    }
    let mut sort_keys: Vec<(Value, bool)> = Vec::with_capacity(order_by.len());
    for ob in order_by {
        let v = eval_scalar(&ob.expr, ctx)?;
        sort_keys.push((v, ob.descending));
    }
    if preserves_null_inputs {
        acc.observe_including_null(value, sort_keys)?;
    } else if order_by.is_empty() {
        acc.observe(value)?;
    } else {
        acc.observe_with_sort_keys(value, sort_keys)?;
    }
    Ok(())
}

pub(in crate::sql) fn is_json_array_aggregate(name: &str) -> bool {
    name.eq_ignore_ascii_case("json_agg") || name.eq_ignore_ascii_case("jsonb_agg")
}

pub(in crate::sql) fn is_json_object_aggregate(name: &str) -> bool {
    name.eq_ignore_ascii_case("json_object_agg") || name.eq_ignore_ascii_case("jsonb_object_agg")
}

pub(in crate::sql) fn is_ordered_set_aggregate(name: &str) -> bool {
    name.eq_ignore_ascii_case("percentile_cont")
        || name.eq_ignore_ascii_case("percentile_disc")
        || name.eq_ignore_ascii_case("mode")
}
