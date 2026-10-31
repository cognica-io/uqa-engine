//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Routine lookup preserves namespace and privilege order while admitting each retained key.

use super::{RoutineNameCatalog, SQLError};
use uqa_core::{
    memory::{Produced, ProductionControl, ProductionVec},
    RelationIdentity, RelationName, RelationReference,
};

pub fn routine_lookup_keys_with_control(
    catalog: &dyn RoutineNameCatalog,
    name: &str,
    control: &ProductionControl<'_>,
) -> Result<Produced<Vec<String>>, SQLError> {
    control.check()?;
    let reference = match RelationIdentity::try_borrowed_reference(name) {
        Ok(reference) => reference,
        Err(error) => {
            let message = control.format(format_args!("invalid routine name `{name}`: {error}"))?;
            let sqlstate = control.copy_text("42602")?;
            return Err(SQLError::Routine {
                sqlstate: sqlstate.into_parts().0,
                message: message.into_parts().0,
            });
        }
    };
    let mut keys = ProductionVec::new(*control);
    if let Some(schema) = reference.schema {
        let mut schema = control.format(format_args!("{schema}"))?;
        if schema.as_str() == "pg_temp" {
            if let Some(temporary) = catalog.temporary_schema_with_control(control)? {
                schema = temporary;
            }
        }
        if !catalog.schema_exists_with_control(&schema, control)? {
            let message = control.format(format_args!(
                "schema \"{}\" does not exist",
                schema.as_str()
            ))?;
            let sqlstate = control.copy_text("3F000")?;
            return Err(SQLError::Routine {
                sqlstate: sqlstate.into_parts().0,
                message: message.into_parts().0,
            });
        }
        catalog.require_schema_usage_with_control(&schema, &catalog.current_role(), control)?;
        keys.push_produced(
            RelationReference {
                schema: Some(RelationName::decoded(&schema)),
                name: reference.name,
            }
            .render_with_control(control)?,
        )?;
        return Ok(keys.finish()?);
    }
    let current_user = catalog.current_role();
    let search_path = catalog.search_path_with_control(control)?;
    let mut implicit_catalog = true;
    for schema in &*search_path {
        control.check()?;
        if schema == "pg_catalog" {
            implicit_catalog = false;
            break;
        }
    }
    for schema in implicit_catalog
        .then_some("pg_catalog")
        .into_iter()
        .chain(search_path.iter().map(String::as_str))
    {
        control.check()?;
        if schema == "pg_temp"
            || crate::catalog::temporary_namespace::is_temporary_schema_name(schema)
            || !catalog.schema_exists_with_control(schema, control)?
            || !catalog.schema_has_usage_with_control(schema, &current_user, control)?
        {
            continue;
        }
        keys.push_produced(
            RelationReference {
                schema: Some(RelationName::decoded(schema)),
                name: reference.name,
            }
            .render_with_control(control)?,
        )?;
    }
    Ok(keys.finish()?)
}

/// Volatility inquiry preserves the ordinary unavailable-catalog fallback while resource failures remain fatal to the invoking planner.
pub fn routine_volatility_lookup_keys_with_control(
    catalog: &dyn RoutineNameCatalog,
    name: &str,
    control: &ProductionControl<'_>,
) -> Result<Option<Produced<Vec<String>>>, SQLError> {
    match routine_lookup_keys_with_control(catalog, name, control) {
        Ok(keys) => Ok(Some(keys)),
        Err(error)
            if matches!(error.primary(), SQLError::Cancelled(_))
                || error.sqlstate() == Some("53200") =>
        {
            Err(error)
        }
        Err(_) => Ok(None),
    }
}

/// Recursive argument analysis defers exactly the same namespace errors as ordinary binding; admission and cancellation remain observable.
pub fn routine_lookup_keys_for_analysis_with_control(
    catalog: &dyn RoutineNameCatalog,
    name: &str,
    control: &ProductionControl<'_>,
) -> Result<Option<Produced<Vec<String>>>, SQLError> {
    match routine_lookup_keys_with_control(catalog, name, control) {
        Err(error) if crate::routines::is_routine_namespace_lookup_error(&error) => Ok(None),
        result => result.map(Some),
    }
}

pub fn require_routine_schema_usage_with_control(
    catalog: &dyn RoutineNameCatalog,
    schema: &str,
    role: &crate::catalog::roles::RoleReference,
    control: &ProductionControl<'_>,
) -> Result<(), SQLError> {
    if catalog.schema_has_usage_with_control(schema, role, control)? {
        return Ok(());
    }
    let message = control.format(format_args!("permission denied for schema {schema}"))?;
    let sqlstate = control.copy_text("42501")?;
    Err(SQLError::Routine {
        sqlstate: sqlstate.into_parts().0,
        message: message.into_parts().0,
    })
}
