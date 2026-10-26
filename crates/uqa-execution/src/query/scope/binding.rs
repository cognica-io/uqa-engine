//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Immutable namespace guard for immediate derived-source metadata transfer.

use super::{CatalogReadView, CteScope, RelationNameResolution};
use crate::RowSchema;
use std::collections::{BTreeMap, BTreeSet};
use uqa_sql::SQLError;

pub(crate) struct SourceBindingScope {
    catalog: CatalogReadView,
    resolution: RelationNameResolution,
    rows: BTreeMap<String, RowSchema>,
    deferred: BTreeMap<String, uqa_sql::plan::RetainedCtePlan>,
    non_returning: BTreeSet<String>,
    recursive_widths: BTreeMap<String, usize>,
}

impl SourceBindingScope {
    pub(crate) fn capture<S: Clone>(ctes: &CteScope<S>) -> Result<Self, SQLError> {
        Ok(Self {
            catalog: ctes.catalog_read_view()?,
            resolution: ctes.relation_name_resolution()?,
            rows: ctes
                .rows
                .iter()
                .map(|(name, rows)| (name.clone(), rows.row_schema().clone()))
                .collect(),
            deferred: ctes.deferred_ctes.clone(),
            non_returning: ctes.non_returning_ctes.clone(),
            recursive_widths: ctes.recursive_control_widths.clone(),
        })
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
            && self.non_returning == ctes.non_returning_ctes
            && self.recursive_widths == ctes.recursive_control_widths
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
