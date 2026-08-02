//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Aggregate expression replacement, input extraction, and observation.

use super::{
    aggregate_value_with_args, distinct_key, eval_scalar, is_aggregate, AggregateAccumulator,
    Engine, SQLError, ScalarEvalContext, ScalarExpr, ScalarOrder, Value,
};

pub(in crate::sql) fn replace_aggregates_with_values(
    engine: &Engine,
    expr: &ScalarExpr,
    accs: &[AggregateAccumulator],
    cursor: &mut usize,
) -> Result<ScalarExpr, SQLError> {
    if is_aggregate(engine, expr) {
        let ScalarExpr::Func { name, args, .. } = expr else {
            return Err(SQLError::Internal("aggregate expr lost".into()));
        };
        let Some(acc) = accs.get(*cursor) else {
            return Err(SQLError::Internal("aggregate accumulator missing".into()));
        };
        *cursor += 1;
        return Ok(ScalarExpr::Literal(aggregate_value_with_args(
            name, acc, args,
        )?));
    }
    match expr {
        ScalarExpr::Func {
            name,
            args,
            distinct,
            order_by,
            filter,
        } => Ok(ScalarExpr::Func {
            name: name.clone(),
            args: args
                .iter()
                .map(|arg| replace_aggregates_with_values(engine, arg, accs, cursor))
                .collect::<Result<Vec<_>, _>>()?,
            distinct: *distinct,
            order_by: order_by.clone(),
            filter: filter
                .as_deref()
                .map(|filter| {
                    replace_aggregates_with_values(engine, filter, accs, cursor).map(Box::new)
                })
                .transpose()?,
        }),
        ScalarExpr::Array(items) => Ok(ScalarExpr::Array(
            items
                .iter()
                .map(|item| replace_aggregates_with_values(engine, item, accs, cursor))
                .collect::<Result<Vec<_>, _>>()?,
        )),
        ScalarExpr::Binary { op, lhs, rhs } => Ok(ScalarExpr::Binary {
            op: *op,
            lhs: Box::new(replace_aggregates_with_values(engine, lhs, accs, cursor)?),
            rhs: Box::new(replace_aggregates_with_values(engine, rhs, accs, cursor)?),
        }),
        ScalarExpr::Not(inner) => Ok(ScalarExpr::Not(Box::new(replace_aggregates_with_values(
            engine, inner, accs, cursor,
        )?))),
        ScalarExpr::And(parts) => Ok(ScalarExpr::And(
            parts
                .iter()
                .map(|part| replace_aggregates_with_values(engine, part, accs, cursor))
                .collect::<Result<Vec<_>, _>>()?,
        )),
        ScalarExpr::Or(parts) => Ok(ScalarExpr::Or(
            parts
                .iter()
                .map(|part| replace_aggregates_with_values(engine, part, accs, cursor))
                .collect::<Result<Vec<_>, _>>()?,
        )),
        ScalarExpr::IsNull { expr, negated } => Ok(ScalarExpr::IsNull {
            expr: Box::new(replace_aggregates_with_values(engine, expr, accs, cursor)?),
            negated: *negated,
        }),
        ScalarExpr::Between { expr, low, high } => Ok(ScalarExpr::Between {
            expr: Box::new(replace_aggregates_with_values(engine, expr, accs, cursor)?),
            low: Box::new(replace_aggregates_with_values(engine, low, accs, cursor)?),
            high: Box::new(replace_aggregates_with_values(engine, high, accs, cursor)?),
        }),
        ScalarExpr::InList {
            expr,
            list,
            negated,
        } => Ok(ScalarExpr::InList {
            expr: Box::new(replace_aggregates_with_values(engine, expr, accs, cursor)?),
            list: list
                .iter()
                .map(|item| replace_aggregates_with_values(engine, item, accs, cursor))
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
                .map(|base| {
                    replace_aggregates_with_values(engine, base, accs, cursor).map(Box::new)
                })
                .transpose()?,
            when: when
                .iter()
                .map(|(condition, result)| {
                    Ok((
                        replace_aggregates_with_values(engine, condition, accs, cursor)?,
                        replace_aggregates_with_values(engine, result, accs, cursor)?,
                    ))
                })
                .collect::<Result<Vec<_>, SQLError>>()?,
            else_branch: else_branch
                .as_deref()
                .map(|branch| {
                    replace_aggregates_with_values(engine, branch, accs, cursor).map(Box::new)
                })
                .transpose()?,
        }),
        ScalarExpr::Cast { expr, ty } => Ok(ScalarExpr::Cast {
            expr: Box::new(replace_aggregates_with_values(engine, expr, accs, cursor)?),
            ty: ty.clone(),
        }),
        ScalarExpr::InSubquery {
            expr,
            subquery,
            negated,
        } => Ok(ScalarExpr::InSubquery {
            expr: Box::new(replace_aggregates_with_values(engine, expr, accs, cursor)?),
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
    aggregate_targets
        .iter()
        .map(|expression| match expression {
            ScalarExpr::Func { name, .. } => {
                Ok(engine.registered_aggregate_function(name).map_or_else(
                    || AggregateAccumulator::builtin_with_budget(name, budget_bytes),
                    |function| AggregateAccumulator::registered_with_budget(function, budget_bytes),
                ))
            }
            _ => Ok(AggregateAccumulator::with_budget(budget_bytes)),
        })
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
        if distinct {
            let key = distinct_key(&Value::List(values.clone()))?;
            if !acc.distinct.insert(key)? {
                return Ok(());
            }
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
    if distinct && (preserves_null_inputs || !matches!(value, Value::Null)) {
        let key = distinct_key(value)?;
        if !acc.distinct.insert(key)? {
            return Ok(());
        }
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
