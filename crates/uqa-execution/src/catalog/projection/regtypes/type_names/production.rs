//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Borrow retained type definitions and admit only the selected result projection.

use super::CATALOG_NAMED_TYPES;
use crate::catalog::CatalogReadView;
use uqa_core::memory::{Produced, ProductionControl};
use uqa_sql::{
    ast::{UserTypeIdentity, UserTypeKind},
    ColumnType, SQLError,
};

pub fn catalog_user_type_name_with_control(
    catalog: &CatalogReadView,
    schema: &str,
    name: &str,
    control: &ProductionControl<'_>,
) -> Result<Option<Produced<ColumnType>>, SQLError> {
    for definition in catalog.ranges() {
        control.check()?;
        for (identity, array, multi) in [
            (&definition.identity, &definition.array_name, false),
            (
                &definition.multirange.identity,
                &definition.multirange.array_name,
                true,
            ),
        ] {
            if identity.schema != schema || (identity.name != name && array != name) {
                continue;
            }
            let ty = if multi {
                definition.multirange_type_with_control(control)?
            } else {
                definition.column_type_with_control(control)?
            };
            return array_type(ty, array == name, control).map(Some);
        }
    }
    for definition in catalog.domains() {
        control.check()?;
        if definition.identity.schema != schema {
            continue;
        }
        let array = definition.array_name.as_deref().map_or_else(
            || {
                uqa_sql::catalog::array_type_names::default_array_type_name_matches(
                    &definition.identity.name,
                    name,
                )
            },
            |array| array == name,
        );
        if definition.identity.name == name || array {
            return array_type(
                definition.column_type_with_control(control)?,
                definition.identity.name != name,
                control,
            )
            .map(Some);
        }
    }
    for definition in catalog.enums() {
        control.check()?;
        if definition.identity.schema == schema
            && (definition.identity.name == name || definition.array_name == name)
        {
            return array_type(
                definition.column_type_with_control(control)?,
                definition.identity.name != name,
                control,
            )
            .map(Some);
        }
    }
    for definition in catalog.composites() {
        control.check()?;
        if definition.identity.schema == schema
            && (definition.identity.name == name || definition.array_name == name)
        {
            return array_type(
                definition.column_type_with_control(control)?,
                definition.identity.name != name,
                control,
            )
            .map(Some);
        }
    }
    crate::catalog::composite_type::relations::by_name_with_control(catalog, schema, name, control)
}

fn array_type(
    ty: Produced<ColumnType>,
    array: bool,
    control: &ProductionControl<'_>,
) -> Result<Produced<ColumnType>, SQLError> {
    if array {
        ColumnType::array_with_control(ty, control).map_err(Into::into)
    } else {
        Ok(ty)
    }
}

pub fn catalog_user_type_identity_with_control(
    catalog: &CatalogReadView,
    identity: UserTypeIdentity,
    control: &ProductionControl<'_>,
) -> Result<Option<Produced<ColumnType>>, SQLError> {
    control.check()?;
    let selected = match identity.kind {
        UserTypeKind::Range => find_checked(
            catalog.ranges(),
            |definition| definition.oid == identity.oid,
            control,
        )?
        .map(|definition| definition.column_type_with_control(control))
        .transpose()?,
        UserTypeKind::Multirange => find_checked(
            catalog.ranges(),
            |definition| definition.multirange.oid == identity.oid,
            control,
        )?
        .map(|definition| definition.multirange_type_with_control(control))
        .transpose()?,
        UserTypeKind::Enum => find_checked(
            catalog.enums(),
            |definition| definition.oid == identity.oid,
            control,
        )?
        .map(|definition| definition.column_type_with_control(control))
        .transpose()?,
        UserTypeKind::Domain => {
            if let Some(definition) = find_checked(
                catalog.domains(),
                |definition| definition.oid == identity.oid,
                control,
            )? {
                Some(definition.column_type_with_control(control)?)
            } else {
                find_checked(
                    CATALOG_NAMED_TYPES.iter(),
                    |ty| matches!(ty, ColumnType::Domain { oid, .. } if *oid == identity.oid),
                    control,
                )?
                .map(|ty| ty.clone_with_control(control))
                .transpose()?
            }
        }
        UserTypeKind::Composite => {
            if let Some(definition) = find_checked(
                catalog.composites(),
                |definition| definition.oid == identity.oid,
                control,
            )? {
                Some(definition.column_type_with_control(control)?)
            } else {
                crate::catalog::composite_type::relations::by_oid_with_control(
                    catalog,
                    identity.oid,
                    control,
                )?
            }
        }
    };
    let Some(mut selected) = selected else {
        return Ok(None);
    };
    for _ in 0..identity.dimensions {
        selected = ColumnType::array_with_control(selected, control)?;
    }
    control.check()?;
    Ok(Some(selected))
}

fn find_checked<'a, T: 'a>(
    values: impl IntoIterator<Item = &'a T>,
    matches: impl Fn(&T) -> bool,
    control: &ProductionControl<'_>,
) -> Result<Option<&'a T>, SQLError> {
    for value in values {
        control.check()?;
        if matches(value) {
            return Ok(Some(value));
        }
    }
    Ok(None)
}
