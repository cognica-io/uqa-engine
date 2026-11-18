//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Stored routine type names select native catalog identities without copying complete type descriptors.

use super::{catalog_user_type_name_identity_from_input, CATALOG_NAMED_TYPES};
use crate::catalog::{
    namespaces::current_schema_names_from_input,
    selection_work::{checkpoint, equal},
    CatalogReadView, RelationNameResolution,
};
use std::{future::Future, ops::Deref};
use uqa_core::{image_io::AsyncImageInput, memory::ProductionControl};
use uqa_sql::{
    ast::{UserTypeIdentity, UserTypeKind},
    ColumnType, SQLError,
};

/// Select a session namespace only after parsing an unqualified or temporary-schema spelling.
pub trait RoutineTypeNamespace: Sync {
    type Namespace<'a>: Deref<Target = RelationNameResolution> + Send
    where
        Self: 'a;
    fn capture<'a>(
        &'a self,
        control: &ProductionControl<'_>,
        input: &mut AsyncImageInput<'_>,
    ) -> impl Future<Output = Result<Self::Namespace<'a>, SQLError>> + Send;
}

impl RoutineTypeNamespace for RelationNameResolution {
    type Namespace<'a> = &'a RelationNameResolution;
    async fn capture<'a>(
        &'a self,
        _: &ProductionControl<'_>,
        _: &mut AsyncImageInput<'_>,
    ) -> Result<Self::Namespace<'a>, SQLError> {
        Ok(self)
    }
}

enum Selection {
    Missing,
    Builtin,
    User(UserTypeIdentity),
}

/// Bind a historical non-identity routine type spelling in one selected catalog/session generation. Built-ins and unresolved names have no user-type identity; existing serialized identities are retained by the declaration binder before this operation.
pub async fn routine_type_name_identity_from_input(
    catalog: &CatalogReadView,
    resolution: &RelationNameResolution,
    type_name: &str,
    control: &ProductionControl<'_>,
    input: &mut AsyncImageInput<'_>,
) -> Result<Option<UserTypeIdentity>, SQLError> {
    Ok(
        match select(catalog, resolution, type_name, control, input).await? {
            Selection::User(identity) => Some(identity),
            Selection::Missing | Selection::Builtin => None,
        },
    )
}

/// Classify an unresolved canonical VARIADIC name using the same selected array identity as a complete type lookup, without copying enum values, domain bases or table descriptors.
pub async fn routine_variadic_array_from_input(
    catalog: &CatalogReadView,
    namespace: &impl RoutineTypeNamespace,
    type_name: &str,
    control: &ProductionControl<'_>,
    input: &mut AsyncImageInput<'_>,
) -> Result<bool, SQLError> {
    if let Some(identity) = UserTypeIdentity::parse_from_input(type_name, control, input)
        .await
        .map_err(|error| match error {
            uqa_sql::ast::TypeIdentityInputError::Work(error) => SQLError::from(error),
            uqa_sql::ast::TypeIdentityInputError::Native(error) => SQLError::from(error),
        })?
    {
        // The SQL declaration already recognized every [] suffix. A bare stored domain, enum, composite or range is never an array, including a domain built on an array.
        return Ok(identity.dimensions != 0);
    }
    let builtin_array = |ty: &ColumnType| matches!(ty, ColumnType::Array(_) | ColumnType::AnyArray);
    if type_name.starts_with("pg_catalog.") || type_name.starts_with("\"pg_catalog\".") {
        if let Some(ty) = super::resolution::optional(
            ColumnType::from_sql_name_from_input(type_name, control, input).await,
        )? {
            return Ok(builtin_array(&ty));
        }
    }
    Ok(
        match select(catalog, namespace, type_name, control, input).await? {
            Selection::User(identity) => identity.dimensions != 0,
            Selection::Missing => false,
            Selection::Builtin => super::resolution::optional(
                ColumnType::from_sql_name_from_input(type_name, control, input).await,
            )?
            .is_some_and(|ty| builtin_array(&ty)),
        },
    )
}

async fn select(
    catalog: &CatalogReadView,
    namespace: &impl RoutineTypeNamespace,
    type_name: &str,
    control: &ProductionControl<'_>,
    input: &mut AsyncImageInput<'_>,
) -> Result<Selection, SQLError> {
    checkpoint(control, input).await?;
    let parsed = match uqa_sql::parse_regtype_name_from_input(type_name, control, input).await {
        Ok(Some(parsed)) => parsed,
        Err(error)
            if matches!(error.primary(), SQLError::Cancelled(_))
                || error.sqlstate() == Some("53200") =>
        {
            return Err(error)
        }
        _ => return Ok(Selection::Missing),
    };
    let (schema, local) = match parsed.names.as_slice() {
        [local] => (None, local.as_str()),
        [schema, local] => (Some(schema.as_str()), local.as_str()),
        _ => return Ok(Selection::Missing),
    };
    let resolution = if schema.is_none() || schema == Some("pg_temp") {
        Some(namespace.capture(control, input).await?)
    } else {
        None
    };
    let qualified = match schema {
        Some("pg_temp") => Some(
            resolution
                .as_deref()
                .expect("temporary namespace")
                .temporary_schema
                .as_str(),
        ),
        value => value,
    };
    let names = if qualified.is_none() {
        let resolution = resolution.as_deref().expect("unqualified namespace");
        Some(
            current_schema_names_from_input(
                catalog,
                resolution,
                &resolution.current_user,
                true,
                control,
                input,
            )
            .await?,
        )
    } else {
        None
    };
    for schema in qualified.into_iter().chain(
        names
            .iter()
            .flat_map(|names| names.iter().map(String::as_str)),
    ) {
        checkpoint(control, input).await?;
        if schema == "pg_catalog"
            && uqa_sql::catalog::type_metadata::builtin_catalog_type_name_exists(local)
        {
            return Ok(Selection::Builtin);
        }
        for shell in catalog.shells() {
            checkpoint(control, input).await?;
            if equal(&shell.identity.schema, schema, control, input).await?
                && equal(&shell.identity.name, local, control, input).await?
            {
                return Ok(Selection::Missing);
            }
        }
        let mut identity =
            catalog_user_type_name_identity_from_input(catalog, schema, local, control, input)
                .await?;
        if identity.is_none() {
            for ty in CATALOG_NAMED_TYPES.iter() {
                checkpoint(control, input).await?;
                if let ColumnType::Domain {
                    schema: owner,
                    name,
                    oid,
                    ..
                } = ty
                {
                    if equal(owner, schema, control, input).await?
                        && equal(name, local, control, input).await?
                    {
                        identity = Some(UserTypeIdentity {
                            kind: UserTypeKind::Domain,
                            oid: *oid,
                            dimensions: 0,
                        });
                        break;
                    }
                }
            }
        }
        if let Some(mut identity) = identity {
            identity.dimensions += parsed.array_dimensions;
            control.check()?;
            return Ok(Selection::User(identity));
        }
    }
    control.check()?;
    Ok(Selection::Missing)
}
