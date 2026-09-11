//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Cursor binding and dependency metadata consumed before snapshot registration.

pub mod dependencies;
pub mod relations;

#[cfg(test)]
mod tests;

use crate::{plan::QueryPlan, routines::RoutineResolution, SQLError};
use std::collections::BTreeSet;
use uqa_core::RelationIdentity;

pub trait PortalRelationCatalog {
    fn try_resolve_table_name(&self, name: &str) -> Result<Option<String>, String>;
    fn hierarchy_scan_tables(
        &self,
        table: &str,
        include_descendants: bool,
    ) -> Result<Vec<String>, SQLError>;
    fn view_plan(&self, name: &str) -> Result<Option<QueryPlan>, SQLError>;
    fn try_resolve_visible_relation_kind(
        &self,
        name: &str,
    ) -> Result<Option<(String, &'static str)>, SQLError>;
    fn resolve_age_label_relation_name(&self, name: &str) -> Result<Option<String>, SQLError>;
    fn try_resolve_sequence_reference(&self, name: &str) -> Result<Option<String>, String>;
}

pub trait PortalTransitionRelations {
    fn active_transition_relation_names(&self) -> BTreeSet<String>;
}

pub struct PortalBindingContext<'a> {
    pub catalog: &'a dyn PortalRelationCatalog,
    pub routines: &'a dyn RoutineResolution,
    pub transitions: &'a dyn PortalTransitionRelations,
}

pub struct SessionPortalTableDependencies {
    pub tables: Option<std::collections::BTreeSet<RelationIdentity>>,
    pub graphs: Option<std::collections::BTreeSet<String>>,
    pub graph_catalog: bool,
}

impl SessionPortalTableDependencies {
    pub fn empty() -> Self {
        Self {
            tables: Some(std::collections::BTreeSet::new()),
            graphs: Some(std::collections::BTreeSet::new()),
            graph_catalog: false,
        }
    }

    pub fn all() -> Self {
        Self {
            tables: None,
            graphs: None,
            graph_catalog: true,
        }
    }

    pub fn is_all(&self) -> bool {
        self.tables.is_none() && self.graphs.is_none()
    }

    pub fn includes(&self, relation: &RelationIdentity) -> bool {
        self.tables
            .as_ref()
            .is_none_or(|tables| tables.contains(relation))
    }

    pub fn insert(&mut self, relation: RelationIdentity) {
        if let Some(relations) = &mut self.tables {
            relations.insert(relation);
        }
    }

    pub fn insert_graph(&mut self, graph: String) {
        self.graph_catalog = true;
        if let Some(graphs) = &mut self.graphs {
            graphs.insert(graph);
        }
    }
}

pub fn prepare_query(
    inputs: &PortalBindingContext<'_>,
    query: &mut QueryPlan,
) -> Result<SessionPortalTableDependencies, SQLError> {
    relations::bind_session_portal_query_relations(
        inputs,
        query,
        &std::collections::BTreeSet::new(),
    )?;
    super::view_dependencies::bind_query_plan_sequence_references(query, &mut |reference| {
        inputs
            .catalog
            .try_resolve_sequence_reference(reference)
            .map_err(|error| {
                SQLError::Internal(format!(
                    "bind cursor sequence `{reference}` at DECLARE: {error}"
                ))
            })
            .and_then(|bound| {
                bound.ok_or_else(|| SQLError::Routine {
                    sqlstate: "42P01".into(),
                    message: format!("relation \"{reference}\" does not exist"),
                })
            })
    })?;
    let table_dependencies = dependencies::session_portal_table_dependencies(inputs, query)?;
    Ok(table_dependencies)
}
