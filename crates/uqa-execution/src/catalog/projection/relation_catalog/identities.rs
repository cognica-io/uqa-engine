//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Relation identities for name and kind lookup, without row counts or expression projection.

use crate::catalog::{CatalogReadView, RelationNameResolution};
use uqa_core::RelationIdentity;
use uqa_sql::SQLError;

pub(in crate::catalog::projection) struct RelationCatalogIdentity {
    pub oid: i64,
    pub relation: RelationIdentity,
    pub kind: &'static str,
}

impl RelationCatalogIdentity {
    fn new(oid: i64, relation: &RelationIdentity, kind: &'static str) -> Self {
        Self {
            oid,
            relation: relation.clone(),
            kind,
        }
    }
}

pub(in crate::catalog::projection) fn relation_catalog_identities(
    catalog: &CatalogReadView,
    resolution: &RelationNameResolution,
) -> Result<Vec<RelationCatalogIdentity>, SQLError> {
    use super::super::{
        ag_catalog, helpers::oids::relation_oid, pg_catalog::catalog_index_relations,
    };
    let snapshot = catalog.snapshot();
    let definitions = &snapshot.definitions;
    let mut out = vec![RelationCatalogIdentity::new(
        13_313,
        &RelationIdentity::new("information_schema", "information_schema_catalog_name"),
        "v",
    )];
    out.extend(snapshot.tables.iter().map(|(name, table)| {
        RelationCatalogIdentity::new(
            i64::from(table.catalog_oids.relation),
            name,
            if table.hierarchy.partition_spec.is_some() {
                "p"
            } else {
                "r"
            },
        )
    }));
    out.extend(definitions.views.iter().map(|(name, view)| {
        RelationCatalogIdentity::new(
            super::super::view_relation_oid(view),
            name,
            match view.kind {
                crate::catalog::view::StoredViewKind::View => "v",
                crate::catalog::view::StoredViewKind::Materialized => "m",
            },
        )
    }));
    out.extend(definitions.foreign_tables.iter().map(|(name, table)| {
        RelationCatalogIdentity::new(super::super::foreign_table_relation_oid(table), name, "f")
    }));
    out.extend(definitions.sequences.keys().map(|name| {
        let object_id = definitions
            .sequence_object_ids
            .get(name)
            .copied()
            .unwrap_or_default();
        RelationCatalogIdentity::new(catalog.sequence_catalog_oid(&object_id), name, "S")
    }));
    out.extend(
        catalog_index_relations(catalog, resolution)?
            .iter()
            .map(|index| RelationCatalogIdentity::new(index.oid(), &index.relation, index.relkind)),
    );
    out.extend(definitions.composites.values().map(|definition| {
        RelationCatalogIdentity::new(
            i64::from(definition.relation_oid),
            &definition.identity,
            "c",
        )
    }));
    for graph in ag_catalog::graph_catalog_entries(catalog)? {
        let oids = catalog.graph_catalog_oids(&graph.name);
        let sequence = RelationIdentity::new(&graph.name, "_label_id_seq");
        out.push(RelationCatalogIdentity::new(
            oids.map_or_else(
                || relation_oid("S", &sequence.schema, &sequence.name),
                |oids| i64::from(oids.label_sequence),
            ),
            &sequence,
            "S",
        ));
        for label in graph.labels {
            out.push(RelationCatalogIdentity::new(
                ag_catalog::label_relation_oid(catalog, &graph.name, &label),
                &RelationIdentity::new(&graph.name, &label.name),
                "r",
            ));
            let sequence =
                RelationIdentity::new(&graph.name, ag_catalog::label_sequence_name(&label.name));
            out.push(RelationCatalogIdentity::new(
                oids.and_then(|oids| oids.labels.get(&label.id))
                    .map_or_else(
                        || relation_oid("S", &sequence.schema, &sequence.name),
                        |oids| i64::from(oids.sequence),
                    ),
                &sequence,
                "S",
            ));
        }
    }
    Ok(out)
}
