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
        .read_graph_names()?
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

pub fn age_label_relation_columns_with_control(
    catalog: &CatalogReadView,
    resolution: &RelationNameResolution,
    name: &str,
    control: &uqa_core::memory::ProductionControl<'_>,
) -> Result<Option<uqa_core::memory::Produced<Vec<String>>>, SQLError> {
    let Some(kind) = age_label_kind_with_control(catalog, resolution, name, control)? else {
        return Ok(None);
    };
    let mut output = uqa_core::memory::ProductionVec::new(*control);
    for column in age_columns(kind) {
        output.push_produced(control.copy_text(column)?)?;
    }
    Ok(Some(output.finish()?))
}

pub fn age_label_relation_schema_with_control(
    catalog: &CatalogReadView,
    resolution: &RelationNameResolution,
    name: &str,
    control: &uqa_core::memory::ProductionControl<'_>,
) -> Result<
    Option<uqa_core::memory::Produced<uqa_sql::catalog::analysis::VirtualRelationSchema>>,
    SQLError,
> {
    let Some(kind) = age_label_kind_with_control(catalog, resolution, name, control)? else {
        return Ok(None);
    };
    let mut output = uqa_core::memory::ProductionVec::new(*control);
    for column in age_columns(kind) {
        let qualified = if *column == "properties" {
            "ag_catalog.agtype"
        } else {
            "ag_catalog.graphid"
        };
        let oid = u32::try_from(uqa_sql::catalog::oids::stable_oid("type", qualified))
            .expect("AGE type OID");
        let (ty, type_memory) = uqa_sql::catalog::system_catalog_domain_with_control(oid, control)?
            .expect("AGE declaration")
            .into_parts();
        let (name, name_memory) = control.copy_text(column)?.into_parts();
        output.push_produced(
            control.finish((name, ty), control.combine(name_memory, type_memory))?,
        )?;
    }
    Ok(Some(output.finish()?))
}

fn age_columns(kind: LabelKind) -> &'static [&'static str] {
    match kind {
        LabelKind::Vertex => &["id", "properties"],
        LabelKind::Edge => &["id", "start_id", "end_id", "properties"],
    }
}

fn age_label_kind_with_control(
    catalog: &CatalogReadView,
    resolution: &RelationNameResolution,
    name: &str,
    control: &uqa_core::memory::ProductionControl<'_>,
) -> Result<Option<LabelKind>, SQLError> {
    control.check()?;
    let reference = RelationIdentity::try_borrowed_reference(name).map_err(|error| {
        control
            .format(format_args!("invalid AGE label relation `{name}`: {error}"))
            .map(|text| SQLError::Internal(text.into_parts().0))
            .unwrap_or_else(Into::into)
    })?;
    let schema = reference
        .schema
        .map(|schema| control.format(format_args!("{schema}")))
        .transpose()?;
    let label = control.format(format_args!("{}", reference.name))?;
    let names = schema
        .as_ref()
        .map(|schema| schema.as_str())
        .into_iter()
        .chain(
            schema
                .is_none()
                .then_some(resolution.search_path())
                .into_iter()
                .flatten()
                .map(String::as_str),
        );
    for graph in names {
        control.check()?;
        let Some(kind) = catalog.graph_label_kind(graph, &label)? else {
            continue;
        };
        return Ok(Some(kind));
    }
    Ok(None)
}

impl AgeLabelRelation {
    pub fn includes_graphid(&self, graphid: u64) -> bool {
        self.label.id == self.label.kind.default_label_id()
            || uqa_graph::graphid_label_id(graphid) == self.label.id
    }
}
