//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Validate native catalog relationships without invoking SQL or reconstructing stored definitions.

use super::CatalogReadSnapshot;
use uqa_core::{memory::MemoryBudget, CancellationToken};
use uqa_sql::{catalog::CatalogWork, SQLError};

mod authority;
pub use authority::validate_authority;

mod relationships;
pub use relationships::{validate as validate_relationships, RelationshipValidation};
mod type_dependencies;
pub use type_dependencies::validate as validate_types;
mod plan_dependencies;
pub use plan_dependencies::validate as validate_plan_relations;
mod event_bindings;
mod event_dependencies;
pub use event_bindings::validate as validate_event_bindings;
mod routine_dependencies;
pub use routine_dependencies::validate as validate_routines;
mod routine_bindings;
pub use routine_bindings::validate as validate_routine_bindings;
mod routine_declarations;
pub use routine_declarations::{
    validate as validate_routine_declarations, DeclarationTypeLookup, VariadicLookup,
};
mod sequence_functions;
pub use sequence_functions::validate as validate_sequence_functions;
mod namespaces;
pub use namespaces::validate as validate_namespaces;
mod name_claims;
pub use name_claims::validate as validate_name_uniqueness;
mod constraint_bindings;
mod scalar_bindings;
mod stored_expressions;
pub use constraint_bindings::validate as validate_constraint_bindings;
pub use stored_expressions::validate as validate_stored_expressions;
mod index_bindings;
pub use index_bindings::validate as validate_index_bindings;
mod partition_bindings;
pub use partition_bindings::validate as validate_partition_bindings;

/// Join complete immutable ordinary/foreign row layouts, domains, constraint triggers and original index rows for SQL's constraint-reference predicate.
pub fn validate_constraints(
    snapshot: &CatalogReadSnapshot,
    memory: &MemoryBudget,
    cancellation: &CancellationToken,
) -> Result<(), SQLError> {
    super::selection_work::inline(validate_constraints_with_work(
        snapshot,
        memory,
        cancellation,
        &mut (),
    ))
}

async fn validate_constraints_with_work(
    snapshot: &CatalogReadSnapshot,
    memory: &MemoryBudget,
    cancellation: &CancellationToken,
    work: &mut (impl CatalogWork + Send),
) -> Result<(), SQLError> {
    use uqa_sql::schema::constraints::native::NativeConstraintTable;
    uqa_sql::schema::constraints::native::validate_with_work(
        snapshot
            .tables
            .iter()
            .map(|(name, table)| NativeConstraintTable {
                name,
                object_id: table.object_id,
                columns: &table.columns,
                checks: &table.checks,
                keys: &table.keys,
                foreign_keys: &table.foreign_keys,
                hierarchy: Some(&table.hierarchy),
            })
            .chain(
                snapshot
                    .definitions
                    .foreign_tables
                    .iter()
                    .map(|(name, table)| NativeConstraintTable {
                        name,
                        object_id: table.object_id,
                        columns: &table.columns,
                        checks: &table.checks,
                        keys: &[],
                        foreign_keys: &[],
                        hierarchy: None,
                    }),
            ),
        snapshot.definitions.catalog_indexes.values(),
        snapshot
            .definitions
            .domains
            .values()
            .map(|domain| &domain.definition),
        snapshot
            .definitions
            .triggers
            .values()
            .flat_map(uqa_sql::catalog::events::EventDefinitions::values),
        memory,
        cancellation,
        work,
    )
    .await
}

/// Join original ordinary-table and index membership for SQL's native ownership predicate.
pub fn validate_indexes(
    snapshot: &CatalogReadSnapshot,
    memory: &MemoryBudget,
    cancellation: &CancellationToken,
) -> Result<(), SQLError> {
    super::selection_work::inline(validate_indexes_with_work(
        snapshot,
        memory,
        cancellation,
        &mut (),
    ))
}

async fn validate_indexes_with_work(
    snapshot: &CatalogReadSnapshot,
    memory: &MemoryBudget,
    cancellation: &CancellationToken,
    work: &mut (impl CatalogWork + Send),
) -> Result<(), SQLError> {
    uqa_sql::catalog::index::native::validate_with_work(
        snapshot.tables.iter().map(|(name, table)| {
            uqa_sql::catalog::index::native::NativeIndexTable {
                name,
                object_id: table.object_id,
                columns: table.columns_declared.then_some(&table.columns),
                keys: &table.keys,
                hierarchy: &table.hierarchy,
            }
        }),
        snapshot.definitions.catalog_indexes.values(),
        memory,
        cancellation,
        work,
    )
    .await
}

/// Join ordinary and foreign column identities with the selected native sequence registry; SQL checks the original dependency forms without migration or value allocation.
pub fn validate_sequences(
    snapshot: &CatalogReadSnapshot,
    memory: &MemoryBudget,
    cancellation: &CancellationToken,
) -> Result<(), SQLError> {
    super::selection_work::inline(validate_sequences_with_work(
        snapshot,
        memory,
        cancellation,
        &mut (),
    ))
}

async fn validate_sequences_with_work(
    snapshot: &CatalogReadSnapshot,
    memory: &MemoryBudget,
    cancellation: &CancellationToken,
    work: &mut (impl CatalogWork + Send),
) -> Result<(), SQLError> {
    uqa_sql::schema::sequences::native::validate_with_work(
        snapshot
            .tables
            .values()
            .map(|table| (table.object_id, table.columns.as_slice()))
            .chain(
                snapshot
                    .definitions
                    .foreign_tables
                    .values()
                    .map(|table| (table.object_id, table.columns.as_slice())),
            ),
        snapshot
            .definitions
            .sequences
            .iter()
            .map(|(name, record)| (name, record.state.owner)),
        memory,
        cancellation,
        work,
    )
    .await
}

/// Validate original table parent references against the complete selected table registry. SQL owns inheritance/partition semantics and the admitted acyclicity traversal.
pub fn validate_hierarchies(
    snapshot: &CatalogReadSnapshot,
    memory: &MemoryBudget,
    cancellation: &CancellationToken,
) -> Result<(), SQLError> {
    super::selection_work::inline(validate_hierarchies_with_work(
        snapshot,
        memory,
        cancellation,
        &mut (),
    ))
}

async fn validate_hierarchies_with_work(
    snapshot: &CatalogReadSnapshot,
    memory: &MemoryBudget,
    cancellation: &CancellationToken,
    work: &mut (impl CatalogWork + Send),
) -> Result<(), SQLError> {
    uqa_sql::schema::inheritance::native::validate_with_work(
        snapshot
            .tables
            .iter()
            .map(|(name, table)| (name, &*table.hierarchy, table.persistence)),
        memory,
        cancellation,
        work,
    )
    .await
}

#[cfg(test)]
mod tests;
