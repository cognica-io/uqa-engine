//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Selected type display borrows immutable catalog definitions and admits only its projected names.

use super::super::type_names::{
    catalog_user_type_identity_with_control, catalog_user_type_name_with_control,
    CATALOG_NAMED_TYPES,
};
use crate::catalog::{context::CatalogContext, CatalogReadView};
use uqa_core::memory::{Produced, ProductionControl};
use uqa_sql::{
    ast::{UserTypeIdentity, UserTypeKind},
    ColumnType, SQLError,
};

pub fn format_type_name_with_control(
    context: &CatalogContext<'_>,
    ty: &ColumnType,
    control: &ProductionControl<'_>,
) -> Result<Produced<String>, SQLError> {
    control.check()?;
    let oid = uqa_sql::catalog::type_metadata::pg_type_oid(ty);
    if oid == 0 {
        return ty.regtype_name_with_control(control).map_err(Into::into);
    }
    let array = matches!(ty, ColumnType::Array(_));
    let mut scalar = ty;
    while let ColumnType::Array(element) = scalar {
        scalar = element;
    }
    let scalar_oid = uqa_sql::catalog::type_metadata::pg_type_oid(scalar);
    let builtin = uqa_sql::catalog::type_metadata::builtin_scalar_type(
        u32::try_from(scalar_oid).unwrap_or(0),
    );
    let selected;
    let catalog = context.catalog.refreshed_catalog_snapshot()?;
    let identity = match scalar {
        ColumnType::Enum(reference) => Some((UserTypeKind::Enum, reference.oid)),
        ColumnType::Composite(reference) => Some((UserTypeKind::Composite, reference.oid)),
        ColumnType::Domain { oid, .. } => Some((UserTypeKind::Domain, *oid)),
        ColumnType::DeclaredRange(reference) => Some((UserTypeKind::Range, reference.oid)),
        ColumnType::DeclaredMultirange(reference) => {
            Some((UserTypeKind::Multirange, reference.oid))
        }
        _ => None,
    };
    let scalar = if let Some((kind, oid)) = identity {
        selected = catalog_user_type_identity_with_control(
            &catalog,
            UserTypeIdentity {
                kind,
                oid,
                dimensions: 0,
            },
            control,
        )?;
        let Some(selected) = selected.as_deref() else {
            return control.copy_text("???").map_err(Into::into);
        };
        selected
    } else if let Some(builtin) = builtin {
        builtin
    } else {
        return control.copy_text("???").map_err(Into::into);
    };
    let (schema, name) = type_namespace(scalar);
    // format_type's SQL keyword spellings are independent of namespace shadowing and ignore declaration typmods.
    let fixed = builtin_display_name(scalar_oid);
    let output = if let Some(fixed) = fixed {
        control.copy_text(fixed)?
    } else {
        let local = if schema == "pg_catalog" {
            scalar.regtype_name_with_control(control)?
        } else {
            uqa_sql::expr::quote_ident_with_control(name, control)?
        };
        let resolution = context
            .session
            .relation_name_resolution_with_control(control)?;
        let names = crate::catalog::namespaces::current_schema_names_with_control(
            &catalog,
            &resolution,
            &context.current_role(),
            true,
            control,
        )?;
        let mut visible = false;
        for candidate in names.iter() {
            if name_exists(&catalog, candidate, name, control)? {
                visible = candidate == schema;
                break;
            }
        }
        let keyword_alias = if !visible && schema == "pg_catalog" {
            uqa_sql::parse_regtype_name_with_control(&local, control)?
                .is_some_and(|parsed| parsed.names == ["pg_catalog", name])
        } else {
            false
        };
        if visible || keyword_alias {
            local
        } else {
            let schema = uqa_sql::expr::quote_ident_with_control(schema, control)?;
            control.format(format_args!("{}.{}", schema.as_str(), local.as_str()))?
        }
    };
    if array {
        control
            .format(format_args!("{}[]", output.as_str()))
            .map_err(Into::into)
    } else {
        Ok(output)
    }
}

fn name_exists(
    catalog: &CatalogReadView,
    schema: &str,
    name: &str,
    control: &ProductionControl<'_>,
) -> Result<bool, SQLError> {
    control.check()?;
    if schema == "pg_catalog"
        && uqa_sql::catalog::type_metadata::builtin_scalar_type_by_name(name).is_some()
    {
        return Ok(true);
    }
    for shell in catalog.shells() {
        control.check()?;
        if shell.identity.schema == schema && shell.identity.name == name {
            return Ok(true);
        }
    }
    for ty in CATALOG_NAMED_TYPES.iter() {
        control.check()?;
        if matches!(ty, ColumnType::Domain { schema: owner, name: local, .. } if owner == schema && local == name)
        {
            return Ok(true);
        }
    }
    Ok(catalog_user_type_name_with_control(catalog, schema, name, control)?.is_some())
}

fn builtin_display_name(oid: i64) -> Option<&'static str> {
    match oid {
        16 => Some("boolean"),
        20 => Some("bigint"),
        21 => Some("smallint"),
        23 => Some("integer"),
        700 => Some("real"),
        701 => Some("double precision"),
        1560 => Some("bit"),
        1042 => Some("character"),
        1043 => Some("character varying"),
        1700 => Some("numeric"),
        1083 => Some("time without time zone"),
        1266 => Some("time with time zone"),
        1114 => Some("timestamp without time zone"),
        1184 => Some("timestamp with time zone"),
        1186 => Some("interval"),
        _ => None,
    }
}

fn type_namespace(scalar: &ColumnType) -> (&str, &str) {
    match scalar {
        ColumnType::Domain { schema, name, .. } => (schema.as_str(), name.as_str()),
        ColumnType::Enum(reference) => (reference.schema.as_str(), reference.name.as_str()),
        ColumnType::Composite(reference) => (reference.schema.as_str(), reference.name.as_str()),
        ColumnType::DeclaredRange(reference) | ColumnType::DeclaredMultirange(reference) => {
            (reference.schema.as_str(), reference.name.as_str())
        }
        _ => (
            "pg_catalog",
            uqa_sql::catalog::type_metadata::builtin_type_name(scalar)
                .expect("implemented builtin type"),
        ),
    }
}
