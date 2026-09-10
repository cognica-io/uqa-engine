//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{CatalogReadView, RelationNameResolution};
use uqa_core::RelationIdentity;
use uqa_graph::{GraphLabelInfo, LabelKind};
use uqa_sql::catalog::{age_agtype, age_graphid};
use uqa_sql::{ColumnType, SQLError};

/// One graph with its `ag_label` entries.
pub struct GraphCatalogEntry {
    pub name: String,
    pub labels: Vec<GraphLabelInfo>,
}

#[derive(Clone)]
pub struct AgeLabelRelation {
    pub graph: String,
    pub label: GraphLabelInfo,
}

impl AgeLabelRelation {
    pub fn canonical_name(&self) -> String {
        format!(
            "{}.{}",
            uqa_sql::expr::quote_ident(&self.graph),
            uqa_sql::expr::quote_ident(&self.label.name)
        )
    }
}

pub fn graph_catalog_entries(
    catalog: &CatalogReadView,
) -> Result<Vec<GraphCatalogEntry>, SQLError> {
    catalog
        .graph_names()
        .into_iter()
        .map(|name| {
            let labels = catalog.graph_labels(&name)?.ok_or_else(|| {
                SQLError::Internal(format!("graph `{name}` disappeared from catalog snapshot"))
            })?;
            Ok(GraphCatalogEntry { name, labels })
        })
        .collect()
}

/// Resolve a graph-label relation through an explicit graph schema or the
/// current `search_path`. Only surviving `ag_label` entries are relations;
/// dropped default and user labels remain absent across reopen.
pub fn resolve_age_label_relation(
    catalog: &CatalogReadView,
    resolution: &RelationNameResolution,
    name: &str,
) -> Result<Option<AgeLabelRelation>, SQLError> {
    let (schema, label_name) = RelationIdentity::parse_reference(name).map_err(|error| {
        SQLError::Internal(format!("invalid AGE label relation `{name}`: {error}"))
    })?;
    let graph_names =
        schema.map_or_else(|| resolution.search_path().to_vec(), |schema| vec![schema]);
    for graph_name in graph_names {
        let Some(labels) = catalog.graph_labels(&graph_name)? else {
            continue;
        };
        if let Some(label) = labels.into_iter().find(|label| label.name == label_name) {
            return Ok(Some(AgeLabelRelation {
                graph: graph_name,
                label,
            }));
        }
    }
    Ok(None)
}

pub fn resolve_age_label_relation_name(
    catalog: &CatalogReadView,
    resolution: &RelationNameResolution,
    name: &str,
) -> Result<Option<String>, SQLError> {
    Ok(resolve_age_label_relation(catalog, resolution, name)?
        .map(|relation| relation.canonical_name()))
}

pub fn age_label_relation_schema(
    catalog: &CatalogReadView,
    resolution: &RelationNameResolution,
    name: &str,
) -> Result<Option<Vec<(String, ColumnType)>>, SQLError> {
    let Some(relation) = resolve_age_label_relation(catalog, resolution, name)? else {
        return Ok(None);
    };
    let columns = match relation.label.kind {
        LabelKind::Vertex => vec![
            ("id".into(), age_graphid()),
            ("properties".into(), age_agtype()),
        ],
        LabelKind::Edge => vec![
            ("id".into(), age_graphid()),
            ("start_id".into(), age_graphid()),
            ("end_id".into(), age_graphid()),
            ("properties".into(), age_agtype()),
        ],
    };
    Ok(Some(columns))
}

impl AgeLabelRelation {
    pub fn includes_graphid(&self, graphid: u64) -> bool {
        self.label.id == self.label.kind.default_label_id()
            || uqa_graph::graphid_label_id(graphid) == self.label.id
    }
}
