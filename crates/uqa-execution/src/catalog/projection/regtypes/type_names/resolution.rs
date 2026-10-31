//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Type-name lookup projects the selected catalog definition without materializing a type catalog.

use super::{
    catalog_user_type_identity_with_control, catalog_user_type_name_with_control,
    CATALOG_NAMED_TYPES,
};
use crate::catalog::{context::CatalogContext, CatalogReadView};
use uqa_core::memory::{Produced, ProductionControl, ProductionString, ProductionVec};
use uqa_sql::{ast::UserTypeIdentity, ColumnType, ParsedRegtypeName, SQLError};

mod diagnostics;
use diagnostics::{routine, QualifiedNames};

fn optional<T>(result: Result<T, SQLError>) -> Result<Option<T>, SQLError> {
    match result {
        Ok(value) => Ok(Some(value)),
        Err(error)
            if matches!(error.primary(), SQLError::Cancelled(_))
                || error.sqlstate() == Some("53200") =>
        {
            Err(error)
        }
        Err(_) => Ok(None),
    }
}

fn schemas(
    context: &CatalogContext<'_>,
    catalog: &CatalogReadView,
    qualified: Option<&str>,
    control: &ProductionControl<'_>,
) -> Result<Produced<Vec<String>>, SQLError> {
    if let Some(schema) = qualified.filter(|schema| *schema != "pg_temp") {
        let mut names = ProductionVec::new(*control);
        names.push_produced(control.copy_text(schema)?)?;
        return Ok(names.finish()?);
    }
    let resolution = context
        .session
        .relation_name_resolution_with_control(control)?;
    if qualified.is_some() {
        let mut names = ProductionVec::new(*control);
        names.push_produced(control.copy_text(&resolution.temporary_schema)?)?;
        Ok(names.finish()?)
    } else {
        crate::catalog::namespaces::current_schema_names_with_control(
            catalog,
            &resolution,
            &context.current_role(),
            true,
            control,
        )
    }
}

pub fn resolve_catalog_column_type_with_control(
    context: &CatalogContext<'_>,
    type_name: &str,
    control: &ProductionControl<'_>,
) -> Result<Option<Produced<ColumnType>>, SQLError> {
    control.check()?;
    if let Some(identity) = UserTypeIdentity::parse(type_name) {
        return catalog_user_type_identity_with_control(
            &context.catalog_read_view(),
            identity,
            control,
        );
    }
    if type_name.starts_with("pg_catalog.") || type_name.starts_with("\"pg_catalog\".") {
        if let Some(ty) = optional(ColumnType::from_sql_name_with_control(type_name, control))? {
            return Ok(Some(ty));
        }
    }
    let Some(parsed) =
        optional(uqa_sql::parse_regtype_name_with_control(type_name, control))?.flatten()
    else {
        return Ok(None);
    };
    resolve_parsed(context, type_name, &parsed, control)
}

fn resolve_parsed(
    context: &CatalogContext<'_>,
    type_name: &str,
    parsed: &ParsedRegtypeName,
    control: &ProductionControl<'_>,
) -> Result<Option<Produced<ColumnType>>, SQLError> {
    let (qualified, local) = match parsed.names.as_slice() {
        [local] => (None, local),
        [schema, local] => (Some(schema.as_str()), local),
        _ => return Ok(None),
    };
    let catalog = context.catalog_read_view();
    let names = schemas(context, &catalog, qualified, control)?;
    for schema in names.iter() {
        control.check()?;
        if schema == "pg_catalog" {
            let builtin =
                if uqa_sql::catalog::type_metadata::builtin_scalar_type_by_name(local).is_some() {
                    true
                } else {
                    let local = uqa_sql::expr::quote_ident_with_control(local, control)?;
                    let qualified = control.format(format_args!("pg_catalog.{}", &**local))?;
                    optional(ColumnType::from_sql_name_with_control(&qualified, control))?.is_some()
                };
            if builtin {
                return optional(ColumnType::from_sql_name_with_control(type_name, control));
            }
        }
        for shell in catalog.shells() {
            control.check()?;
            if shell.identity.schema == *schema && shell.identity.name == *local {
                return Ok(None);
            }
        }
        let mut resolved = catalog_user_type_name_with_control(&catalog, schema, local, control)?;
        if resolved.is_none() {
            for ty in CATALOG_NAMED_TYPES.iter() {
                control.check()?;
                if matches!(ty, ColumnType::Domain { schema: owner, name, .. } if owner == schema && name == local)
                {
                    resolved = Some(ty.clone_with_control(control)?);
                    break;
                }
            }
        }
        if let Some(mut ty) = resolved {
            for _ in 0..parsed.array_dimensions {
                ty = ColumnType::array_with_control(ty, control)?;
            }
            return Ok(Some(ty));
        }
    }
    Ok(None)
}

fn quoted_name(
    parsed: &ParsedRegtypeName,
    control: &ProductionControl<'_>,
) -> Result<Produced<String>, SQLError> {
    let mut output = ProductionString::new(*control);
    for (index, name) in parsed.names.iter().enumerate() {
        if index != 0 {
            output.push('.')?;
        }
        output.push('"')?;
        for character in name.chars() {
            if character == '"' {
                output.push('"')?;
            }
            output.push(character)?;
        }
        output.push('"')?;
    }
    Ok(output.finish()?)
}

pub fn resolve_catalog_column_type_name_with_control(
    context: &CatalogContext<'_>,
    type_name: &str,
    control: &ProductionControl<'_>,
) -> Result<Produced<ColumnType>, SQLError> {
    if let Some(identity) = UserTypeIdentity::parse(type_name) {
        return resolve_catalog_column_type_with_control(context, type_name, control)?.ok_or_else(
            || {
                routine(
                    "42704",
                    format_args!("type with OID {} does not exist", identity.oid),
                    control,
                )
            },
        );
    }
    let parsed = uqa_sql::parse_regtype_name_with_control(type_name, control)?;
    if let Some(parsed) = parsed.as_ref().filter(|parsed| parsed.has_type_modifiers) {
        let name = quoted_name(parsed, control)?;
        if resolve_catalog_column_type_with_control(context, &name, control)?
            .is_some_and(|ty| matches!(*ty, ColumnType::Domain { .. }))
        {
            return Err(routine(
                "42601",
                format_args!(
                    "type modifier is not allowed for type \"{}\"",
                    QualifiedNames(&parsed.names)
                ),
                control,
            ));
        }
    }
    if let Some(ty) = resolve_catalog_column_type_with_control(context, type_name, control)? {
        return Ok(ty);
    }
    validate_missing_type(context, parsed.as_deref(), control)?;
    Err(if let Some(name) = parsed.as_ref() {
        routine(
            "42704",
            format_args!(
                "type \"{}{}\" does not exist",
                QualifiedNames(&name.names),
                if name.array_dimensions > 0 { "[]" } else { "" },
            ),
            control,
        )
    } else {
        routine(
            "42704",
            format_args!("type \"{type_name}\" does not exist"),
            control,
        )
    })
}

fn validate_missing_type(
    context: &CatalogContext<'_>,
    parsed: Option<&ParsedRegtypeName>,
    control: &ProductionControl<'_>,
) -> Result<(), SQLError> {
    let Some(parsed) = parsed else {
        return Ok(());
    };
    let (qualified, local) = match parsed.names.as_slice() {
        [name] => (None, name.as_str()),
        [schema, name] => (Some(schema.as_str()), name.as_str()),
        [_, _, _] => return Err(diagnostics::cross_database(&parsed.names, control)),
        _ => {
            return Err(routine(
                "42601",
                format_args!(
                    "improper qualified name (too many dotted names): {}",
                    QualifiedNames(&parsed.names),
                ),
                control,
            ));
        }
    };
    let catalog = context.catalog.refreshed_catalog_snapshot()?;
    let names = schemas(context, &catalog, qualified, control)?;
    if let Some(schema) = names.first().filter(|_| qualified.is_some()) {
        let resolution = context
            .session
            .relation_name_resolution_with_control(control)?;
        if !crate::catalog::namespaces::namespace_schema_exists(
            &catalog,
            &resolution.temporary_schema,
            schema,
        ) {
            return Err(routine(
                "3F000",
                format_args!("schema \"{schema}\" does not exist"),
                control,
            ));
        }
        crate::catalog::namespaces::require_schema_privilege_with_control(
            &catalog,
            &resolution.temporary_schema,
            schema,
            &context.current_role(),
            crate::catalog::security::schema::SchemaAclPrivilege::Usage,
            control,
        )?;
    }
    if parsed.array_dimensions == 0 {
        for schema in names.iter() {
            for shell in catalog.shells() {
                control.check()?;
                if shell.identity.schema == *schema && shell.identity.name == local {
                    return Err(routine(
                        "42704",
                        format_args!("type \"{}\" is only a shell", QualifiedNames(&parsed.names)),
                        control,
                    ));
                }
            }
            if catalog_user_type_name_with_control(&catalog, schema, local, control)?.is_some() {
                break;
            }
        }
    }
    Ok(())
}
