//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Type input selects the first namespace identity before applying array syntax or shell validation.

use super::{
    object_name, routine, schemas, CatalogContext, CatalogReadView, ProductionControl,
    QualifiedNames, SQLError,
};
use crate::catalog::{namespaces, security::schema::SchemaAclPrivilege};
use uqa_sql::{
    catalog::type_metadata::{pg_type_array_oid, pg_type_oid},
    ParsedRegtypeName,
};

pub(super) fn resolve(
    context: &CatalogContext<'_>,
    input: &str,
    control: &ProductionControl<'_>,
) -> Result<Option<i64>, SQLError> {
    let Some(parsed) = uqa_sql::parse_regtype_name_with_control(input, control)? else {
        return Ok(None);
    };
    parsed_oid(context, &parsed, control)
}

pub(super) fn parsed_oid(
    context: &CatalogContext<'_>,
    parsed: &ParsedRegtypeName,
    control: &ProductionControl<'_>,
) -> Result<Option<i64>, SQLError> {
    let (qualified, local) = object_name(&parsed.names, control)?;
    let resolution = context
        .session
        .relation_name_resolution_with_control(control)?;
    let qualified = qualified.map(|schema| {
        if schema == "pg_temp" {
            resolution.temporary_schema.as_str()
        } else {
            schema
        }
    });
    let catalog = context.catalog_read_view();
    if let Some(schema) = qualified {
        if !namespaces::namespace_schema_exists(&catalog, &resolution.temporary_schema, schema) {
            return Err(routine(
                "3F000",
                format_args!("schema \"{schema}\" does not exist"),
                control,
            ));
        }
        namespaces::require_schema_privilege_with_control(
            &catalog,
            &resolution.temporary_schema,
            schema,
            &resolution.current_user,
            SchemaAclPrivilege::Usage,
            control,
        )?;
    }
    for schema in schemas(context, qualified, control)?.iter() {
        control.check()?;
        let Some((oid, array, defined)) = identity(&catalog, schema, local, control)? else {
            continue;
        };
        let selected = if parsed.array_dimensions == 0 {
            oid
        } else {
            array
        };
        if selected == 0 {
            return Ok(None);
        }
        if !defined {
            return Err(routine(
                "42704",
                format_args!("type \"{}\" is only a shell", QualifiedNames(&parsed.names)),
                control,
            ));
        }
        return Ok(Some(selected));
    }
    Ok(None)
}

fn identity(
    catalog: &CatalogReadView,
    schema: &str,
    name: &str,
    control: &ProductionControl<'_>,
) -> Result<Option<(i64, i64, bool)>, SQLError> {
    if schema == "pg_catalog" {
        if let Some((oid, array)) =
            crate::catalog::projection::pg_catalog::builtin_type_identity_by_name(name, control)?
        {
            return Ok(Some((oid, array, true)));
        }
    }
    for shell in catalog.shells() {
        control.check()?;
        if shell.identity.schema == schema && shell.identity.name == name {
            return Ok(Some((i64::from(shell.oid), 0, false)));
        }
    }
    if let Some(ty) = super::super::type_names::catalog_user_type_name_with_control(
        catalog, schema, name, control,
    )? {
        return Ok(Some((pg_type_oid(&ty), pg_type_array_oid(&ty), true)));
    }
    for ty in super::super::type_names::CATALOG_NAMED_TYPES.iter() {
        control.check()?;
        let (owner, local) = match ty {
            uqa_sql::ColumnType::Domain { schema, name, .. } => (schema.as_str(), name.as_str()),
            _ => continue,
        };
        if owner == schema {
            if local == name {
                return Ok(Some((pg_type_oid(ty), pg_type_array_oid(ty), true)));
            }
            if name.strip_prefix('_') == Some(local) {
                return Ok(Some((pg_type_array_oid(ty), 0, true)));
            }
        }
    }
    Ok(None)
}
