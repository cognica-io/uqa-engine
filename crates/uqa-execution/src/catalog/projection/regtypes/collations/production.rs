//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Collation selection borrows catalog ACLs and admits only parsed names and namespace workspace.

use super::{builtin_collation_oid, input_error, CatalogContext, SQLError, SchemaAclPrivilege};
use crate::catalog::namespaces;
use uqa_core::memory::ProductionControl;

pub fn resolve_expression_collation_oid_with_control(
    context: &CatalogContext<'_>,
    input: &str,
    control: &ProductionControl<'_>,
) -> Result<u32, SQLError> {
    let oid = resolve_named_collation(context, input, false, control)?;
    u32::try_from(oid).map_err(|_| SQLError::Internal("invalid collation identity".into()))
}

pub(super) fn resolve_named_collation(
    context: &CatalogContext<'_>,
    input: &str,
    missing_schema_ok: bool,
    control: &ProductionControl<'_>,
) -> Result<i64, SQLError> {
    let names = uqa_sql::parse_regobject_name_with_control(input, control)?
        .ok_or_else(|| input_error("42602", "invalid name syntax".into()))?;
    let (schema, local) = match names.as_slice() {
        [local] => (None, local),
        [schema, local] => (Some(schema.as_str()), local),
        [database, schema, local] if database == "uqa" => (Some(schema.as_str()), local),
        [_, _, _] => {
            return Err(formatted_error(
                format_args!(
                    "cross-database references are not implemented: {}",
                    QualifiedNames(&names)
                ),
                control,
                SQLError::Unsupported,
            ));
        }
        _ => {
            return Err(formatted_error(
                format_args!(
                    "improper qualified name (too many dotted names): {}",
                    QualifiedNames(&names),
                ),
                control,
                |message| input_error("42601", message),
            ))
        }
    };
    let visible = match schema {
        Some(schema) => visible_in_schema(context, schema, missing_schema_ok, control)?,
        None => {
            let catalog = context.catalog.refreshed_catalog_snapshot()?;
            let resolution = context
                .session
                .relation_name_resolution_with_control(control)?;
            namespaces::current_schema_names_with_control(
                &catalog,
                &resolution,
                &resolution.current_user,
                true,
                control,
            )?
            .iter()
            .any(|schema| schema == "pg_catalog")
        }
    };
    if let Some(oid) = visible.then(|| builtin_collation_oid(local)).flatten() {
        return Ok(oid);
    }
    Err(formatted_error(
        format_args!(
            "collation \"{}\" for encoding \"UTF8\" does not exist",
            QualifiedNames(&names),
        ),
        control,
        |message| input_error("42704", message),
    ))
}

fn visible_in_schema(
    context: &CatalogContext<'_>,
    schema: &str,
    missing_ok: bool,
    control: &ProductionControl<'_>,
) -> Result<bool, SQLError> {
    let resolution = context
        .session
        .relation_name_resolution_with_control(control)?;
    if schema == "pg_temp" && resolution.temporary_namespace_allocated {
        return Ok(false);
    }
    let allocated =
        schema != resolution.temporary_schema || resolution.temporary_namespace_allocated;
    let catalog = context.catalog_read_view();
    if allocated
        && namespaces::namespace_schema_exists(&catalog, &resolution.temporary_schema, schema)
    {
        namespaces::require_schema_privilege_with_control(
            &catalog,
            &resolution.temporary_schema,
            schema,
            &resolution.current_user,
            SchemaAclPrivilege::Usage,
            control,
        )?;
    } else if !missing_ok {
        return Err(formatted_error(
            format_args!("schema \"{schema}\" does not exist"),
            control,
            |message| input_error("3F000", message),
        ));
    }
    Ok(schema == "pg_catalog")
}

struct QualifiedNames<'a>(&'a [String]);

impl std::fmt::Display for QualifiedNames<'_> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        for (index, name) in self.0.iter().enumerate() {
            if index > 0 {
                formatter.write_str(".")?;
            }
            formatter.write_str(name)?;
        }
        Ok(())
    }
}

fn formatted_error(
    message: std::fmt::Arguments<'_>,
    control: &ProductionControl<'_>,
    error: impl FnOnce(String) -> SQLError,
) -> SQLError {
    match control.format(message) {
        Ok(message) => {
            let (message, _memory) = message.into_parts();
            error(message)
        }
        Err(error) => error.into(),
    }
}
