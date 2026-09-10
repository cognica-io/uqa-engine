//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Aggregate expression replacement, input extraction, and observation.

use super::{
    eval_scalar, AggregateAccumulator, AggregateAccumulatorTemplate, SQLError, ScalarEvalContext,
    ScalarExpr, ScalarOrder, Value,
};

pub fn aggregate_input_value(
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
    if name.eq_ignore_ascii_case("string_agg") {
        let [value, delimiter] = args else {
            return Err(SQLError::TypeMismatch(
                "string_agg requires 2 arguments".into(),
            ));
        };
        let value = eval_scalar(value, ctx)?;
        let delimiter = eval_scalar(delimiter, ctx)?;
        return Ok(if matches!(value, Value::Null) {
            Value::Null
        } else {
            Value::List(vec![value, delimiter])
        });
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

pub fn aggregate_input_values(
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

pub fn new_aggregate_accumulators_with_budget(
    context: &dyn crate::functions::AggregateFunctionRegistry,
    aggregate_targets: &[ScalarExpr],
    input_schema: &crate::RowSchema,
    params: &[uqa_sql::SQLParam],
    budget_bytes: usize,
) -> Result<Vec<AggregateAccumulator>, SQLError> {
    Ok(instantiate_aggregate_accumulators(
        &aggregate_accumulator_templates(context, aggregate_targets, input_schema, params)?,
        budget_bytes,
    ))
}

pub fn aggregate_accumulator_templates(
    context: &dyn crate::functions::AggregateFunctionRegistry,
    aggregate_targets: &[ScalarExpr],
    input_schema: &crate::RowSchema,
    params: &[uqa_sql::SQLParam],
) -> Result<Vec<AggregateAccumulatorTemplate>, SQLError> {
    aggregate_targets
        .iter()
        .map(|expression| match expression {
            ScalarExpr::Func { name, args, .. } => {
                if let Some(function) = context.registered_aggregate_function(name) {
                    return Ok(AggregateAccumulatorTemplate::registered(function));
                }
                let input_type = args
                    .first()
                    .map(|argument| {
                        crate::scalar_type_with_resolver(argument, input_schema, params, context)
                    })
                    .transpose()?
                    .flatten();
                Ok(AggregateAccumulatorTemplate::builtin(
                    name,
                    input_type.as_ref(),
                ))
            }
            _ => Ok(AggregateAccumulatorTemplate::generic()),
        })
        .collect()
}

pub fn instantiate_aggregate_accumulators(
    templates: &[AggregateAccumulatorTemplate],
    budget_bytes: usize,
) -> Vec<AggregateAccumulator> {
    templates
        .iter()
        .map(|template| template.instantiate(budget_bytes))
        .collect()
}

pub fn observe_aggregate(
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

pub fn observe_builtin_aggregate_value(
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

pub fn is_json_array_aggregate(name: &str) -> bool {
    name.eq_ignore_ascii_case("json_agg") || name.eq_ignore_ascii_case("jsonb_agg")
}

pub fn is_json_object_aggregate(name: &str) -> bool {
    name.eq_ignore_ascii_case("json_object_agg") || name.eq_ignore_ascii_case("jsonb_object_agg")
}

pub fn is_ordered_set_aggregate(name: &str) -> bool {
    name.eq_ignore_ascii_case("percentile_cont")
        || name.eq_ignore_ascii_case("percentile_disc")
        || name.eq_ignore_ascii_case("mode")
}
