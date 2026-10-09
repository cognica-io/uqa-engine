//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Work requested from a virtual catalog before its physical column selection.

use std::collections::BTreeSet;

#[derive(Default)]
pub(crate) struct CatalogRequest {
    columns: Option<BTreeSet<String>>,
    names: Vec<(String, String)>,
}

impl CatalogRequest {
    pub(crate) fn columns(columns: impl IntoIterator<Item = String>) -> Self {
        Self {
            columns: Some(columns.into_iter().collect()),
            names: Vec::new(),
        }
    }

    pub(crate) fn includes(&self, column: &str) -> bool {
        self.columns
            .as_ref()
            .is_none_or(|columns| columns.contains(column))
    }

    pub(crate) fn require_name(&mut self, column: String, value: String) {
        self.names.push((column, value));
    }

    pub(crate) fn matches_name(&self, column: &str, value: &str) -> bool {
        self.names
            .iter()
            .filter(|(name, _)| name == column)
            .all(|(_, expected)| expected == value)
    }

    pub(crate) fn matches_relation(&self, schema: &str, relation: &str) -> bool {
        self.matches_name("table_schema", schema) && self.matches_name("table_name", relation)
    }
}
