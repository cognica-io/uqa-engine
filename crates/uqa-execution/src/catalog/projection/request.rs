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
}

impl CatalogRequest {
    pub(crate) fn columns(columns: impl IntoIterator<Item = String>) -> Self {
        Self {
            columns: Some(columns.into_iter().collect()),
        }
    }

    pub(crate) fn includes(&self, column: &str) -> bool {
        self.columns
            .as_ref()
            .is_none_or(|columns| columns.contains(column))
    }
}
