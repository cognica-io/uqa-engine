//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Shared relation binding over names and immutable definition registries.

use super::{
    CatalogDefinitionSnapshot, RelationLookupMode, RelationNameResolution, RelationResolution,
};
use std::collections::{BTreeMap, BTreeSet};
use uqa_core::RelationIdentity;
use uqa_sql::SQLError;

mod production;

/// Table membership is sufficient to bind a relation; callers need not load table definitions.
pub trait RelationTableNames {
    fn contains_table(&self, relation: &RelationIdentity) -> bool;
}

impl RelationTableNames for BTreeSet<RelationIdentity> {
    fn contains_table(&self, relation: &RelationIdentity) -> bool {
        self.contains(relation)
    }
}

impl<T> RelationTableNames for BTreeMap<RelationIdentity, T> {
    fn contains_table(&self, relation: &RelationIdentity) -> bool {
        self.contains_key(relation)
    }
}

impl<T> RelationTableNames for uqa_core::memory::OwnedMap<RelationIdentity, T> {
    fn contains_table(&self, relation: &RelationIdentity) -> bool {
        self.contains_key(relation)
    }
}

/// The shared SQL namespace, borrowing table names without copying their metadata.
pub struct RelationNameCatalog<'a> {
    tables: &'a dyn RelationTableNames,
    definitions: &'a CatalogDefinitionSnapshot,
    graph_names: Option<&'a BTreeSet<String>>,
}

impl<'a> RelationNameCatalog<'a> {
    pub fn new(
        tables: &'a dyn RelationTableNames,
        definitions: &'a CatalogDefinitionSnapshot,
    ) -> Self {
        Self {
            tables,
            definitions,
            graph_names: None,
        }
    }

    /// Use the statement's graph namespace overlay without materializing graph handles.
    pub fn with_graph_names(mut self, names: &'a BTreeSet<String>) -> Self {
        self.graph_names = Some(names);
        self
    }

    /// Produce the only relation candidate set exposed to SQL binding and execution. Dynamic names are filtered by namespace `USAGE`; stored bindings must already be canonical and therefore bypass name lookup entirely.
    pub(super) fn relation_lookup_candidates(
        &self,
        resolution: &RelationNameResolution,
        name: &str,
    ) -> Result<Vec<uqa_core::RelationIdentity>, SQLError> {
        Ok(self
            .relation_lookup_candidates_with_control(
                resolution,
                name,
                &uqa_core::memory::ProductionControl::uncontrolled(),
            )?
            .into_uncontrolled()
            .expect("ordinary relation candidate production"))
    }

    fn namespace_exists(&self, resolution: &RelationNameResolution, schema: &str) -> bool {
        if schema == resolution.temporary_schema {
            return resolution.temporary_namespace_allocated;
        }
        uqa_sql::catalog::is_virtual_system_schema(schema)
            || self.definitions.schemas.contains_key(schema)
            || self.graph_names.map_or_else(
                || self.definitions.graphs.contains_key(schema),
                |names| names.contains(schema),
            )
    }

    pub fn relation_kind_resolution(
        &self,
        resolution: &RelationNameResolution,
        name: &str,
    ) -> Result<RelationResolution, SQLError> {
        Ok(self
            .relation_kind_resolution_with_control(
                resolution,
                name,
                &uqa_core::memory::ProductionControl::uncontrolled(),
            )?
            .into_uncontrolled()
            .expect("ordinary relation resolution production"))
    }
}

#[cfg(test)]
mod production_tests;
#[cfg(test)]
mod tests;
