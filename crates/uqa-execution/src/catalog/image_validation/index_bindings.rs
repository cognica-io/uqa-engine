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
    binding::production::{
        BindingMap, BindingNameSet, BindingResolution, ControlledBindingContext,
    },
    catalog::{
        index::{
            native::{validate_bindings, IndexBindingContext},
            stored::input::definition,
        },
        resolution::{
            bound_relation_identity_with_control, RelationLookupMode, RelationNameResolution,
        },
        roles::RoleReference,
    },
    routines::{native::NativeRoutineReferences, RoutineResolution},
    SQLError,
};

pub fn validate(
    catalog: &CatalogReadView,
    routines: &dyn RoutineResolution,
    memory: &MemoryBudget,
    cancellation: &CancellationToken,
) -> Result<(), SQLError> {
    let control = ProductionControl::new(memory, cancellation, cancellation);
    control.check()?;
    let snapshot = catalog.snapshot();
    let references = NativeRoutineReferences::new(
        snapshot
            .definitions
            .sql_user_functions
            .values()
            .flatten()
            .map(|routine| &routine.def),
        memory,
        cancellation,
    )?;
    let types = super::type_dependencies::references(snapshot, memory, cancellation)?;
    // Scalar index expressions have no query or session namespace. Their declared row and concrete routine bindings supply every lookup.
    let binding = ControlledBindingContext {
        catalog: control
            .finish(catalog.clone(), control.empty_reservation())?
            .into_shared(&control)?,
        resolution: BindingResolution::from_produced(control.finish(
            RelationNameResolution {
                search_path: Vec::new(),
                temporary_schema: String::new(),
                temporary_namespace_allocated: false,
                current_user: RoleReference::Named(String::new()),
                lookup_mode: RelationLookupMode::Bound,
            },
            control.empty_reservation(),
        )?),
        ctes: BindingMap::new(),
        deferred_ctes: BindingMap::new(),
        non_returning_ctes: BindingNameSet::new(),
        scalar_subqueries: &[],
    };
    let context = IndexBindingContext {
        routines,
        references: &references,
        types: &types,
        binding: &binding,
    };
    for row in snapshot.definitions.catalog_indexes.values() {
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
        validate_bindings(
            &keys,
            &definition,
            table.columns_declared.then_some(&table.columns),
            &context,
            &control,
        )?;
    }
    control.check()?;
    Ok(())
}
