//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Range canonicalization and explicit coercion invoke the selected catalog routine with an already admitted range.

use super::context::RoutineInvocationContext;
use crate::catalog::CatalogReadView;
use uqa_core::{
    memory::{Produced, ProductionControl, ProductionVec},
    Value,
};
use uqa_sql::{ast::ColumnType, routines::resolution::RoutineCallKind, SQLError};

pub fn call_range_canonical(
    context: &RoutineInvocationContext<'_>,
    catalog: &CatalogReadView,
    oid: u32,
    value: &Value,
    control: &ProductionControl<'_>,
) -> Result<Produced<Value>, SQLError> {
    control.check()?;
    let Value::Datum(datum) = value else {
        return Err(SQLError::Internal(
            "canonical callback requires a physical range".into(),
        ));
    };
    let definition = catalog
        .range_definition_by_oid(datum.type_oid())?
        .filter(|range| range.oid == datum.type_oid())
        .ok_or_else(|| {
            SQLError::Internal(format!(
                "cache lookup failed for range type {}",
                datum.type_oid()
            ))
        })?;
    invoke(
        context,
        catalog,
        definition,
        RangeFunction::Canonical(oid),
        value,
        control,
    )
}

/// Range declarations publish a function cast using the one-member multirange constructor.
pub fn call_range_cast(
    context: &RoutineInvocationContext<'_>,
    catalog: &CatalogReadView,
    source: u32,
    target: u32,
    value: &Value,
    control: &ProductionControl<'_>,
) -> Result<Option<Produced<Value>>, SQLError> {
    control.check()?;
    let Some(definition) = catalog.range_definition_by_oid(source)? else {
        return Ok(None);
    };
    let Some(cast) = definition
        .cast()
        .filter(|cast| cast.source == source && cast.target == target)
    else {
        return Ok(None);
    };
    if matches!(value, Value::Null) {
        return Ok(Some(control.copy_value(value)?));
    }
    invoke(
        context,
        catalog,
        definition,
        RangeFunction::Cast(cast.function),
        value,
        control,
    )
    .map(Some)
}

#[derive(Clone, Copy)]
enum RangeFunction {
    Canonical(u32),
    Cast(u32),
}

fn invoke(
    context: &RoutineInvocationContext<'_>,
    catalog: &CatalogReadView,
    definition: &uqa_sql::catalog::range_type::StoredRange,
    selected: RangeFunction,
    value: &Value,
    control: &ProductionControl<'_>,
) -> Result<Produced<Value>, SQLError> {
    let oid = match selected {
        RangeFunction::Canonical(oid) | RangeFunction::Cast(oid) => oid,
    };
    let missing = || SQLError::Routine {
        sqlstate: "XX000".into(),
        message: format!("cache lookup failed for function {oid}"),
    };
    let function = catalog.sql_function_by_oid(oid)?.ok_or_else(missing)?;
    let binding = function.catalog_binding(None, control)?;
    let matched = context
        .overloads
        .resolve_static_sql_routine_match(
            &binding.name,
            Some(&binding),
            &[None],
            &[Some(ColumnType::DeclaredRange(definition.reference()))],
            false,
            RoutineCallKind::Function,
        )?
        .ok_or_else(missing)?;
    if matched.function.def.returns_set() {
        return Err(SQLError::Routine {
            sqlstate: "0A000".into(),
            message: "set-valued function called in context that cannot accept a set".into(),
        });
    }
    // The range constructor has already admitted and typed the argument. Recoercion would repeat input or canonical effects.
    let mut arguments = ProductionVec::new(*control);
    arguments.push_produced(control.copy_value(value)?)?;
    let (arguments, _argument_memory) = arguments.finish()?.into_parts();
    let outcome = match selected {
        RangeFunction::Canonical(_) => super::execution::execute_catalog_routine(
            context,
            &matched.function,
            arguments,
            &matched.invocation,
        ),
        RangeFunction::Cast(_) => super::execution::execute_routine(
            &context.with_production_control(*control),
            &matched.function,
            arguments,
            &matched.invocation,
            false,
            None,
        ),
    }?;
    let result = if outcome.out_values.len() == 1 {
        outcome
            .out_values
            .into_iter()
            .next()
            .expect("one OUT result")
    } else {
        outcome.value
    };
    Ok(control.retain_external_value(result)?)
}
