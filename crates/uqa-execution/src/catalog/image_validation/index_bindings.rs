//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Join original index definitions, row layouts and routine identities for SQL's scalar validator.

use crate::catalog::CatalogReadView;
use uqa_core::{
    memory::{MemoryBudget, ProductionControl},
    CancellationToken,
};
use uqa_sql::{
    ast::IndexKey,
    catalog::{
        index::{
            native::{validate_bindings_with_work, IndexBindingContext},
            stored::input::definition,
        },
        resolution::bound_relation_identity_with_control,
    },
    routines::RoutineResolution,
    SQLError,
};

pub fn validate(
    catalog: &CatalogReadView,
    routines: &dyn RoutineResolution,
    memory: &MemoryBudget,
    cancellation: &CancellationToken,
) -> Result<(), SQLError> {
    crate::catalog::selection_work::inline(validate_with_work(
        catalog,
        routines,
        memory,
        cancellation,
        &mut (),
    ))
}

pub(super) async fn validate_with_work(
    catalog: &CatalogReadView,
    routines: &dyn RoutineResolution,
    memory: &MemoryBudget,
    cancellation: &CancellationToken,
    work: &mut (impl uqa_sql::catalog::CatalogWork + Send),
) -> Result<(), SQLError> {
    let control = ProductionControl::new(memory, cancellation, cancellation);
    control.check()?;
    let snapshot = catalog.snapshot();
    let scalar =
        super::scalar_bindings::ScalarBindings::with_work(catalog, memory, cancellation, work)
            .await?;
    for row in snapshot.definitions.catalog_indexes.values() {
        work.checkpoint().await?;
        control.check()?;
        let name = bound_relation_identity_with_control(&row.table_name, &control)?;
        let table = snapshot
            .tables
            .get(&*name)
            .ok_or_else(|| SQLError::Internal("missing native index table".into()))?;
        let keys = IndexKey::read_stored(&row.columns_json, memory, cancellation, cancellation)?;
        let definition = definition(
            row.definition_json.as_deref(),
            memory,
            cancellation,
            cancellation,
        )?;
        let row_type = table
            .catalog_oids
            .row_type_reference_with_control(&name, &control)?
            .map(|reference| {
                let (reference, memory) = reference.into_parts();
                control.finish(uqa_sql::ColumnType::Composite(reference), memory)
            })
            .transpose()?;
        let context = IndexBindingContext {
            relation: Some(&name),
            row_type: row_type.as_deref(),
            routines,
            references: &scalar.references,
            types: &scalar.types,
            binding: &scalar.binding,
        };
        validate_bindings_with_work(
            &keys,
            &definition,
            table.columns_declared.then_some(&table.columns),
            &context,
            &control,
            work,
        )
        .await?;
    }
    control.check()?;
    scalar.retire(work).await
}
