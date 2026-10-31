//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Stored view descriptors retain the selected catalog generation and invoking namespace owner.

use super::super::{view::StoredView, CatalogReadView};
use super::CatalogContext;
use uqa_core::memory::ProductionControl;
use uqa_sql::{
    binding::production::{
        BindingMap, BindingNameSet, BindingResolution, ControlledBindingContext,
    },
    RowSchema, SQLError,
};

impl CatalogContext<'_> {
    pub fn stored_view_schema_with_catalog_with_control(
        &self,
        view: &StoredView,
        catalog: CatalogReadView,
        control: &ProductionControl<'_>,
    ) -> Result<RowSchema, SQLError> {
        control.check()?;
        let catalog = control
            .finish(catalog, control.empty_reservation())?
            .into_shared(control)?;
        let resolution = self
            .session
            .relation_name_resolution_with_control(control)?;
        let scope = ControlledBindingContext {
            catalog,
            resolution: BindingResolution::from_produced(resolution),
            ctes: BindingMap::new(),
            deferred_ctes: BindingMap::new(),
            non_returning_ctes: BindingNameSet::new(),
            scalar_subqueries: &[],
        };
        view.rewrite_definition_ref()
            .row_schema_with_control(self.routines, scope, control)
    }
}
