//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Immutable namespace guard for immediate derived-source metadata transfer.

use super::{production::resolution::ScopeResolution, CatalogReadView, CteScope};
use crate::RowSchema;
use std::sync::Arc;
use uqa_core::memory::{Produced, ProductionControl};
use uqa_sql::{
    binding::production::{BindingMap, BindingNameSet},
    SQLError,
};

pub(crate) struct SourceBindingScope {
    catalog: CatalogReadView,
    resolution: ScopeResolution,
    rows: BindingMap<RowSchema>,
    deferred: BindingMap<uqa_sql::plan::RetainedCtePlan>,
    non_returning: BindingNameSet,
    recursive_widths: BindingMap<usize>,
}

impl SourceBindingScope {
    pub(crate) fn capture<S: Clone>(
        ctes: &CteScope<S>,
        control: &ProductionControl<'_>,
    ) -> Result<Arc<Produced<Self>>, SQLError> {
        control.check()?;
        let mut captured = Self {
            catalog: ctes.catalog_read_view()?,
            resolution: ctes.name_resolution()?.retain_with_control(control)?,
            rows: BindingMap::new(),
            deferred: BindingMap::new(),
            non_returning: BindingNameSet::new(),
            recursive_widths: BindingMap::new(),
        };
        for (name, rows) in &ctes.rows {
            captured
                .rows
                .insert_with_control(name, rows.row_schema().clone(), control)?;
        }
        for (name, plan) in &ctes.deferred_ctes {
            captured
                .deferred
                .insert_with_control(name, plan.clone(), control)?;
        }
        for name in &ctes.non_returning_ctes {
            captured.non_returning.insert_with_control(name, control)?;
        }
        for (name, width) in &ctes.recursive_control_widths {
            captured
                .recursive_widths
                .insert_with_control(name, *width, control)?;
        }
        super::production::shared(captured, control)
    }

    pub(crate) fn matches<S: Clone>(&self, ctes: &CteScope<S>) -> bool {
        let (Some(catalog), Some(resolution)) = (&ctes.catalog, &ctes.catalog_resolution) else {
            return false;
        };
        // Each borrowed query selects its own bound/dynamic relation mode, exactly as it did during recursive SQL binding. All other namespace inputs must still match.
        std::ptr::eq(self.catalog.snapshot(), catalog.snapshot())
            && self.resolution.search_path == resolution.search_path
            && self.resolution.temporary_schema == resolution.temporary_schema
            && self.resolution.temporary_namespace_allocated
                == resolution.temporary_namespace_allocated
            && self.resolution.current_user == resolution.current_user
            && self.non_returning.iter().eq(ctes.non_returning_ctes.iter())
            && self
                .recursive_widths
                .iter()
                .eq(ctes.recursive_control_widths.iter())
            && self.rows.len() == ctes.rows.len()
            && self
                .rows
                .iter()
                .zip(&ctes.rows)
                .all(|((name, schema), (current_name, rows))| {
                    name == current_name && schema == rows.row_schema()
                })
            && self.deferred.len() == ctes.deferred_ctes.len()
            && self.deferred.iter().zip(&ctes.deferred_ctes).all(
                |((name, plan), (current_name, current))| {
                    name == current_name && std::ptr::eq(plan.as_ref(), current.as_ref())
                },
            )
    }
}

#[cfg(test)]
mod tests;
