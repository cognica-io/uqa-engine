//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Canonical callbacks retain their catalog identity and invoke the existing routine owner with already admitted arguments.

use super::context::RoutineInvocationContext;
use crate::catalog::CatalogReadView;
use uqa_core::{
    memory::{Produced, ProductionControl, ProductionVec},
    Value,
};
use uqa_sql::{
    ast::{ColumnType, FunctionBinding},
    routines::{resolution::RoutineCallKind, routine_signature_types},
    SQLError,
};

pub fn call_range_canonical(
    context: &RoutineInvocationContext<'_>,
    catalog: &CatalogReadView,
    oid: u32,
    value: &Value,
    control: &ProductionControl<'_>,
) -> Result<Produced<Value>, SQLError> {
    control.check()?;
    let missing = || SQLError::Routine {
        sqlstate: "XX000".into(),
        message: format!("cache lookup failed for function {oid}"),
    };
    let function = catalog.sql_function_by_oid(oid)?.ok_or_else(missing)?;
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
    let binding = FunctionBinding {
        object_id: function.def.object_id,
        name: function.def.name.clone(),
        argument_types: routine_signature_types(&function.def),
        builtin: false,
        dispatch: None,
        invocation: None,
        composite_field: None,
        resolution_error: None,
        input_collation: None,
    };
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
    let outcome = super::execution::execute_catalog_routine(
        context,
        &matched.function,
        arguments,
        &matched.invocation,
    )?;
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
