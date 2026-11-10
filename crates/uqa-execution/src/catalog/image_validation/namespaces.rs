//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Native declarations retain their original catalog namespace without search-path resolution.

use crate::catalog::{namespaces::namespace_schema_exists, CatalogReadView};
use uqa_core::memory::ProductionControl;
use uqa_sql::{catalog::resolution::bound_relation_identity_with_control, SQLError};

/// Validate the namespace of each declared relation, type and routine against the same immutable catalog. Graph namespaces and virtual system schemas use the ordinary catalog predicate; no session temporary namespace is synthesized for a restored image.
pub fn validate(
    catalog: &CatalogReadView,
    control: &ProductionControl<'_>,
) -> Result<(), SQLError> {
    control.check()?;
    let schema = |name: &str| {
        control.check()?;
        if !namespace_schema_exists(catalog, "", name) {
            return Err(SQLError::Internal(
                "missing native catalog namespace".into(),
            ));
        }
        Ok(())
    };
    let snapshot = catalog.snapshot();
    let d = &snapshot.definitions;
    for relation in snapshot
        .tables
        .keys()
        .chain(d.views.keys())
        .chain(d.foreign_tables.keys())
        .chain(d.sequences.iter().map(|(name, _)| name))
        .chain(d.catalog_indexes.iter().map(|(name, _)| name))
    {
        schema(&relation.schema)?;
    }
    macro_rules! types {
        ($($field:ident),+ $(,)?) => {$(
            for definition in d.$field.values() {
                schema(&definition.identity.schema)?;
            }
        )+};
    }
    types!(domains, enums, composites, ranges, shells);
    for range in d.ranges.values() {
        schema(&range.multirange.identity.schema)?;
    }
    for routine in d.sql_user_functions.values().flatten() {
        let identity = bound_relation_identity_with_control(&routine.def.name, control)?;
        schema(&identity.schema)?;
    }
    control.check()?;
    Ok(())
}

#[cfg(test)]
mod tests;
