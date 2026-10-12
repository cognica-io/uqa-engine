//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Observe analyzed argument metadata after the ordinary scalar scheduler evaluates its value.

use super::EngineHook;
use crate::{ColumnType, SQLError};
use uqa_core::{
    memory::{Produced, ProductionControl},
    Value,
};

#[cfg(test)]
mod tests;

pub(super) fn evaluate(
    collation_oid: u32,
    noncollatable_type_oid: Option<u32>,
    engine: Option<&dyn EngineHook>,
    control: &ProductionControl<'_>,
) -> Result<Produced<Value>, SQLError> {
    control.check()?;
    if let Some(oid) = noncollatable_type_oid {
        let name = catalog_name(&ColumnType::Regtype, oid, engine, control)?;
        return Err(SQLError::Routine {
            sqlstate: "42804".into(),
            message: format!("collations are not supported by type {}", &*name),
        });
    }
    if collation_oid == 0 {
        return Ok(control.finish(Value::Null, control.empty_reservation())?);
    }
    let (name, memory) =
        catalog_name(&ColumnType::Regcollation, collation_oid, engine, control)?.into_parts();
    Ok(control.finish(Value::Str(name), memory)?)
}

fn catalog_name(
    alias: &ColumnType,
    oid: u32,
    engine: Option<&dyn EngineHook>,
    control: &ProductionControl<'_>,
) -> Result<Produced<String>, SQLError> {
    if let Some(engine) = engine {
        if let Some(name) = engine
            .resolve_regtype_output(alias, i64::from(oid))
            .map_err(SQLError::Internal)?
        {
            let (value, memory) = control
                .retain_external_value(Value::Str(name))?
                .into_parts();
            let Value::Str(name) = value else {
                unreachable!("catalog output remains text")
            };
            return Ok(control.finish(name, memory)?);
        }
    }
    if *alias == ColumnType::Regcollation {
        if let Some(name) = crate::catalog::collations::builtin_collation_name(i64::from(oid)) {
            return Ok(super::quote_ident_with_control(name, control)?);
        }
    } else {
        if let Some(ty) = crate::catalog::type_metadata::builtin_scalar_type(oid) {
            return ty.sql_name_with_control(control).map_err(Into::into);
        }
        if let Some(element) = crate::catalog::type_metadata::builtin_array_element(oid) {
            let name = element.sql_name_with_control(control)?;
            return Ok(control.format(format_args!("{}[]", &*name))?);
        }
    }
    Ok(control.format(format_args!("{oid}"))?)
}
