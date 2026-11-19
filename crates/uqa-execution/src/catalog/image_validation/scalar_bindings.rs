//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Borrow original routine/type owners and retain an immutable scalar binding catalog.

use crate::catalog::CatalogReadView;
use uqa_core::{
    memory::{MemoryBudget, ProductionControl},
    CancellationToken,
};
use uqa_sql::{
    binding::production::{
        BindingMap, BindingNameSet, BindingResolution, ControlledBindingContext,
    },
    catalog::{
        resolution::{RelationLookupMode, RelationNameResolution},
        roles::RoleReference,
        type_references::TypeReferences,
    },
    routines::native::NativeRoutineReferences,
    SQLError,
};

pub(super) struct ScalarBindings<'a> {
    pub references: NativeRoutineReferences<'a>,
    pub types: TypeReferences<'a>,
    pub binding: ControlledBindingContext<'a>,
}

impl<'a> ScalarBindings<'a> {
    pub(super) async fn with_work(
        catalog: &'a CatalogReadView,
        memory: &'a MemoryBudget,
        cancellation: &'a CancellationToken,
        work: &mut (impl uqa_sql::catalog::CatalogWork + Send),
    ) -> Result<Self, SQLError> {
        let control = ProductionControl::new(memory, cancellation, cancellation);
        control.check()?;
        let snapshot = catalog.snapshot();
        let references = NativeRoutineReferences::from_groups_with_work(
            snapshot
                .definitions
                .sql_user_functions
                .values()
                .map(Vec::as_slice)
                .map(super::routine_dependencies::group),
            memory,
            cancellation,
            work,
        )
        .await?;
        let types =
            super::type_dependencies::references_with_work(snapshot, memory, cancellation, work)
                .await?;
        // Stored scalar declarations have no query or session namespace. Their declared row and concrete routine bindings supply every lookup.
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
        Ok(Self {
            references,
            types,
            binding,
        })
    }
    pub(super) async fn retire(
        self,
        work: &mut (impl uqa_sql::catalog::CatalogWork + Send),
    ) -> Result<(), SQLError> {
        self.references.retire_with_work(work).await?;
        self.types.retire(work).await
    }
}
