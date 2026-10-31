//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Hierarchy names borrow immutable table identities and admit selected names and traversal nodes.

use super::{CatalogReadView, RelationNameResolution, SQLError};
use crate::catalog::hierarchy::hierarchy_scan_entries_with_control;
use uqa_core::memory::{Produced, ProductionControl};

impl CatalogReadView {
    pub fn hierarchy_scan_tables_with_control(
        &self,
        resolution: &RelationNameResolution,
        name: &str,
        include_descendants: bool,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<Vec<String>>, SQLError> {
        let candidates = self
            .relation_names()
            .relation_lookup_candidates_with_control(resolution, name, control)?;
        let root = candidates
            .iter()
            .find(|relation| self.snapshot.tables.contains_key(*relation))
            .ok_or_else(|| match control.copy_text(name) {
                Ok(name) => SQLError::UnknownTable(name.into_parts().0),
                Err(error) => error.into(),
            })?;
        hierarchy_scan_entries_with_control(
            root,
            include_descendants,
            || self.snapshot.tables.iter(),
            |table, parent| {
                table
                    .hierarchy
                    .parents
                    .iter()
                    .any(|candidate| candidate == parent)
            },
            control,
        )
    }
}
