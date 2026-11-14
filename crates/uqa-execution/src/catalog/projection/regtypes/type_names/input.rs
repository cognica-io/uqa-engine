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
use uqa_core::{image_io::AsyncImageInput, memory::ProductionControl};
use uqa_sql::{
    ast::{UserTypeIdentity, UserTypeKind},
    ColumnType, SQLError,
};

/// Bind a historical non-identity routine type spelling in one selected catalog/session generation. Built-ins and unresolved names have no user-type identity; existing serialized identities are retained by the declaration binder before this operation.
pub async fn routine_type_name_identity_from_input(
    catalog: &CatalogReadView,
    resolution: &RelationNameResolution,
    type_name: &str,
    control: &ProductionControl<'_>,
    input: &mut AsyncImageInput<'_>,
) -> Result<Option<UserTypeIdentity>, SQLError> {
    checkpoint(control, input).await?;
    let parsed = match uqa_sql::parse_regtype_name_from_input(type_name, control, input).await {
        Ok(Some(parsed)) => parsed,
        Err(error)
            if matches!(error.primary(), SQLError::Cancelled(_))
                || error.sqlstate() == Some("53200") =>
        {
            return Err(error)
        }
        _ => return Ok(None),
    };
    let (qualified, local) = match parsed.names.as_slice() {
        [local] => (None, local.as_str()),
        [schema, local] => (
            Some(if schema == "pg_temp" {
                resolution.temporary_schema.as_str()
            } else {
                schema.as_str()
            }),
            local.as_str(),
        ),
        _ => return Ok(None),
    };
    let names = if qualified.is_none() {
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
            return Ok(None);
        }
        for shell in catalog.shells() {
            checkpoint(control, input).await?;
            if equal(&shell.identity.schema, schema, control, input).await?
                && equal(&shell.identity.name, local, control, input).await?
            {
                return Ok(None);
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
            return Ok(Some(identity));
        }
    }
    control.check()?;
    Ok(None)
}
