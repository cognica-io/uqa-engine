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
    let name = uqa_sql::semantics::builtin_aggregate_dispatch_name(name);
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
                    return Err(json_object_aggregate_null_key(name));
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
    instantiate_aggregate_accumulators(
        &aggregate_accumulator_templates(context, aggregate_targets, input_schema, params)?,
        budget_bytes,
    )
}

pub fn aggregate_accumulator_templates(
    context: &dyn crate::functions::AggregateFunctionRegistry,
    aggregate_targets: &[ScalarExpr],
    input_schema: &crate::RowSchema,
    params: &[uqa_sql::SQLParam],
) -> Result<Vec<AggregateAccumulatorTemplate>, SQLError> {
    let has_enums = context
        .enum_labels()
        .is_some_and(uqa_sql::expr::enums::EnumLabelCatalog::has_enum_types);
    aggregate_targets
        .iter()
        .map(|expression| match expression {
            ScalarExpr::Func {
                name,
                args,
                distinct,
                order_by,
                ..
            } => {
                let mut ordering_types = order_by
                    .iter()
                    .map(|key| {
                        crate::scalar_type_with_resolver(&key.expr, input_schema, params, context)
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                if let Some(function) = context.registered_aggregate_function(name) {
                    let mut template = AggregateAccumulatorTemplate::registered(function);
                    if *distinct {
                        ordering_types.extend(
                            args.iter()
                                .map(|argument| {
                                    crate::scalar_type_with_resolver(
                                        argument,
                                        input_schema,
                                        params,
                                        context,
                                    )
                                })
                                .collect::<Result<Vec<_>, _>>()?,
                        );
                        template = template.with_distinct_arguments(args.len());
                    }
                    return Ok(template
                        .with_ordering(if has_enums { ordering_types.len() } else { 0 })
                        .with_ordering_types(ordering_types));
                }
                let name = uqa_sql::semantics::builtin_aggregate_dispatch_name(name);
                let input_type = args
                    .first()
                    .map(|argument| {
                        crate::scalar_type_with_resolver(argument, input_schema, params, context)
                    })
                    .transpose()?
                    .flatten();
                let mut template = AggregateAccumulatorTemplate::builtin(name, input_type.as_ref());
                if *distinct {
                    if is_json_object_aggregate(name) || name.eq_ignore_ascii_case("string_agg") {
                        ordering_types.push(input_type.clone());
                        ordering_types.extend(
                            args.iter()
                                .skip(1)
                                .map(|argument| {
                                    crate::scalar_type_with_resolver(
                                        argument,
                                        input_schema,
                                        params,
                                        context,
                                    )
                                })
                                .collect::<Result<Vec<_>, _>>()?,
                        );
                        template = template.with_distinct_arguments(args.len());
                    } else {
                        ordering_types.push(if is_ordered_set_aggregate(name) {
                            ordering_types.first().cloned().flatten()
                        } else {
                            input_type.clone()
                        });
                    }
                }
                Ok(template
                    .with_ordering(if has_enums { ordering_types.len() } else { 0 })
                    .with_ordering_types(ordering_types))
            }
            _ => Ok(AggregateAccumulatorTemplate::generic()),
        })
        .collect()
}

pub fn instantiate_aggregate_accumulators(
    templates: &[AggregateAccumulatorTemplate],
    budget_bytes: usize,
) -> Result<Vec<AggregateAccumulator>, SQLError> {
    let mut accumulators = Vec::with_capacity(templates.len());
    for template in templates {
        match template.instantiate(budget_bytes) {
            Ok(accumulator) => accumulators.push(accumulator),
            Err(error) => {
                return super::state::with_cleanup(
                    Err(error),
                    super::state::close_accumulators(&mut accumulators),
                )
            }
        }
    }
    Ok(accumulators)
}

pub fn observe_aggregate(
    acc: &mut AggregateAccumulator,
    name: &str,
    args: &[ScalarExpr],
    distinct: bool,
    order_by: &[ScalarOrder],
    ctx: &ScalarEvalContext<'_>,
) -> Result<(), SQLError> {
    let enums = ctx
        .function_hook()
        .and_then(uqa_sql::expr::EngineHook::enum_labels);
    if acc.registered.is_some() {
        let values = uqa_sql::expr::enums::render_host_arguments(
            ctx.function_hook()
                .and_then(uqa_sql::expr::EngineHook::enum_labels),
            &aggregate_input_values(args, ctx)?,
        )?;
        let mut sort_keys: Vec<super::ordering::AggregateSortKey> =
            Vec::with_capacity(order_by.len());
        for ob in order_by {
            let v = eval_scalar(&ob.expr, ctx)?;
            sort_keys.push(super::ordering::AggregateSortKey::ordered(v, ob));
        }
        if distinct {
            return acc.distinct.insert(&Value::List(values), sort_keys, enums);
        }
        acc.observe_registered(values, sort_keys, enums)?;
        return Ok(());
    }

    let value = aggregate_input_value(name, args, order_by, ctx)?;
    observe_builtin_aggregate_value(acc, &value, distinct, order_by, ctx)
}

/// The accumulator's state plan decides whether a NULL input reaches the aggregate.
pub fn observe_builtin_aggregate_value(
    acc: &mut AggregateAccumulator,
    value: &Value,
    distinct: bool,
    order_by: &[ScalarOrder],
    ctx: &ScalarEvalContext<'_>,
) -> Result<(), SQLError> {
    let enums = ctx
        .function_hook()
        .and_then(uqa_sql::expr::EngineHook::enum_labels);
    let mut sort_keys: Vec<super::ordering::AggregateSortKey> = Vec::with_capacity(order_by.len());
    for ob in order_by {
        let v = eval_scalar(&ob.expr, ctx)?;
        sort_keys.push(super::ordering::AggregateSortKey::ordered(v, ob));
    }
    if distinct {
        if acc.admits_null_input() || !matches!(value, Value::Null) {
            acc.distinct.insert(value, sort_keys, enums)?;
        }
        return Ok(());
    }
    if order_by.is_empty() {
        acc.observe_with_enum_catalog(value, enums)
    } else {
        acc.observe_with_sort_keys(value, sort_keys, enums)
    }
}

pub fn is_json_array_aggregate(name: &str) -> bool {
    name.eq_ignore_ascii_case("json_agg") || name.eq_ignore_ascii_case("jsonb_agg")
}

/// The transition-function error of `json_object_agg` and `jsonb_object_agg` for a NULL key.
pub(super) fn json_object_aggregate_null_key(name: &str) -> SQLError {
    if name.eq_ignore_ascii_case("jsonb_object_agg") {
        SQLError::Routine {
            sqlstate: "22023".into(),
            message: "field name must not be null".into(),
        }
    } else {
        SQLError::Routine {
            sqlstate: "22004".into(),
            message: "null value not allowed for object key".into(),
        }
    }
}

pub fn is_json_object_aggregate(name: &str) -> bool {
    name.eq_ignore_ascii_case("json_object_agg") || name.eq_ignore_ascii_case("jsonb_object_agg")
}

pub fn is_ordered_set_aggregate(name: &str) -> bool {
    name.eq_ignore_ascii_case("percentile_cont")
        || name.eq_ignore_ascii_case("percentile_disc")
        || name.eq_ignore_ascii_case("mode")
}
