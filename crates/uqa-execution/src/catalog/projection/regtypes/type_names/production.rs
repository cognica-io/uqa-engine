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
    let selected = crate::catalog::selection_work::inline(super::selection::select(
        catalog,
        schema,
        name,
        control,
        &mut (),
    ))?;
    selected
        .map(|selected| selected.project(control))
        .transpose()
        .map(Option::flatten)
}

pub(super) fn array_type(
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
