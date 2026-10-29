//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Type OID observation borrows definitions and constructs only the selected descriptor.

use super::{CatalogContext, ColumnType, CATALOG_NAMED_TYPES};
use uqa_core::memory::{Produced, ProductionControl};
use uqa_sql::{
    catalog::type_metadata::{pg_domain_array_oid, pg_type_array_oid, pg_type_oid},
    SQLError,
};

pub fn resolve_catalog_user_type_by_oid_with_control(
    context: &CatalogContext<'_>,
    oid: u32,
    control: &ProductionControl<'_>,
) -> Result<Option<Produced<ColumnType>>, SQLError> {
    control.check()?;
    let catalog = context.catalog_read_view();
    for definition in catalog.domains() {
        control.check()?;
        if let Some(array) = selected(
            oid,
            i64::from(definition.oid),
            pg_domain_array_oid(definition.oid, definition.array_oid),
        ) {
            return finish(
                definition.column_type_with_control(control)?,
                array,
                control,
            );
        }
    }
    for definition in catalog.enums() {
        control.check()?;
        if let Some(array) = selected(
            oid,
            i64::from(definition.oid),
            i64::from(definition.array_oid),
        ) {
            return finish(
                definition.column_type_with_control(control)?,
                array,
                control,
            );
        }
    }
    for definition in catalog.composites() {
        control.check()?;
        if let Some(array) = selected(
            oid,
            i64::from(definition.oid),
            i64::from(definition.array_oid),
        ) {
            return finish(
                definition.column_type_with_control(control)?,
                array,
                control,
            );
        }
    }
    for ty in CATALOG_NAMED_TYPES.iter() {
        control.check()?;
        if let Some(array) = selected(oid, pg_type_oid(ty), pg_type_array_oid(ty)) {
            return finish(ty.clone_with_control(control)?, array, control);
        }
    }
    for definition in catalog.ranges() {
        control.check()?;
        if let Some(array) = selected(
            oid,
            i64::from(definition.oid),
            i64::from(definition.array_oid),
        ) {
            return finish(
                definition.column_type_with_control(control)?,
                array,
                control,
            );
        }
        if let Some(array) = selected(
            oid,
            i64::from(definition.multirange.oid),
            i64::from(definition.multirange.array_oid),
        ) {
            return finish(
                definition.multirange_type_with_control(control)?,
                array,
                control,
            );
        }
    }
    crate::catalog::composite_type::relations::by_oid_with_control(&catalog, oid, control)
}

fn selected(oid: u32, scalar: i64, array: i64) -> Option<bool> {
    if i64::from(oid) == scalar {
        Some(false)
    } else if i64::from(oid) == array {
        Some(true)
    } else {
        None
    }
}

fn finish(
    ty: Produced<ColumnType>,
    array: bool,
    control: &ProductionControl<'_>,
) -> Result<Option<Produced<ColumnType>>, SQLError> {
    Ok(Some(if array {
        ColumnType::array_with_control(ty, control)?
    } else {
        ty
    }))
}

#[cfg(test)]
mod tests;
