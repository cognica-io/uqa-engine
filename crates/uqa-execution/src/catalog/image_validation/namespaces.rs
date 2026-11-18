//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Native declarations retain their original catalog namespace without search-path resolution.

use crate::catalog::{
    namespaces::{namespace_schema_exists_with_work, NamespaceWork},
    selection_work, CatalogReadView,
};
use uqa_core::{image_io::AsyncImageInput, memory::ProductionControl, CancellationToken};
use uqa_sql::{
    catalog::{resolution::bound_relation_identity_with_work, CatalogWork},
    SQLError,
};

/// Validate the namespace of each declared relation, type and routine against the same immutable catalog. Graph namespaces and virtual system schemas use the ordinary catalog predicate; no session temporary namespace is synthesized for a restored image.
pub fn validate(
    catalog: &CatalogReadView,
    control: &ProductionControl<'_>,
) -> Result<(), SQLError> {
    selection_work::inline(validate_with_work(catalog, control, &mut ()))
}

pub(super) async fn from_input(
    catalog: &CatalogReadView,
    original: &CancellationToken,
    input: &mut AsyncImageInput<'_>,
) -> Result<(), SQLError> {
    let control = ProductionControl::new(input.memory, original, input.cancellation);
    validate_with_work(catalog, &control, input).await
}

async fn validate_with_work(
    catalog: &CatalogReadView,
    control: &ProductionControl<'_>,
    work: &mut (impl CatalogWork + NamespaceWork),
) -> Result<(), SQLError> {
    control.check()?;
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
        schema(catalog, &relation.schema, control, work).await?;
    }
    macro_rules! types {
        ($($field:ident),+ $(,)?) => {$(
            for definition in d.$field.values() {
                schema(catalog, &definition.identity.schema, control, work).await?;
            }
        )+};
    }
    types!(domains, enums, composites, ranges, shells);
    for range in d.ranges.values() {
        schema(catalog, &range.multirange.identity.schema, control, work).await?;
    }
    for overloads in d.sql_user_functions.values() {
        selection_work::checkpoint(control, work).await?;
        for routine in overloads {
            let identity =
                bound_relation_identity_with_work(&routine.def.name, control, work).await?;
            schema(catalog, &identity.schema, control, work).await?;
        }
    }
    control.check()?;
    Ok(())
}

async fn schema(
    catalog: &CatalogReadView,
    name: &str,
    control: &ProductionControl<'_>,
    work: &mut impl NamespaceWork,
) -> Result<(), SQLError> {
    if !namespace_schema_exists_with_work(catalog, "", name, control, work).await? {
        return Err(SQLError::Internal(
            "missing native catalog namespace".into(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests;
