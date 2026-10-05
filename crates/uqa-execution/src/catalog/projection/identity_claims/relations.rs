//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Relation-class address claims from unfiltered definitions, without table statistics or row scans.

use crate::catalog::{CatalogReadView, RelationNameResolution};
use uqa_core::RelationIdentity;
use uqa_sql::SQLError;

pub(crate) struct RelationClaim {
    pub relation: RelationIdentity,
    pub object_id: Option<[u8; 16]>,
    pub oid: i64,
}

pub(crate) fn relation_claims(
    catalog: &CatalogReadView,
    resolution: &RelationNameResolution,
) -> Result<Vec<RelationClaim>, SQLError> {
    collect_relation_claims(catalog, resolution, false)
}

pub(crate) fn legacy_relation_claims(
    catalog: &CatalogReadView,
    resolution: &RelationNameResolution,
) -> Result<Vec<RelationClaim>, SQLError> {
    collect_relation_claims(catalog, resolution, true)
}

fn collect_relation_claims(
    catalog: &CatalogReadView,
    resolution: &RelationNameResolution,
    legacy: bool,
) -> Result<Vec<RelationClaim>, SQLError> {
    let snapshot = catalog.snapshot();
    let definitions = &snapshot.definitions;
    let mut claims = uqa_sql::catalog::SystemRelation::all()
        .map(|relation| RelationClaim {
            relation: RelationIdentity::new(relation.namespace(), relation.name()),
            object_id: Some(relation.object_id()),
            oid: relation.oid(),
        })
        .collect::<Vec<_>>();
    let mut append = |relation: &RelationIdentity, object_id: [u8; 16], oid: i64| {
        claims.push(RelationClaim {
            relation: relation.clone(),
            object_id: Some(object_id),
            oid,
        });
    };
    for (relation, table) in &snapshot.tables {
        append(
            relation,
            table.object_id,
            i64::from(table.catalog_oids.relation),
        );
    }
    for (relation, view) in definitions.views.iter() {
        append(
            relation,
            view.object_id,
            super::super::view_relation_oid(view),
        );
    }
    for (relation, table) in definitions.foreign_tables.iter() {
        append(
            relation,
            table.object_id,
            super::super::foreign_table_relation_oid(table),
        );
    }
    for (relation, object_id) in definitions.sequence_object_ids.iter() {
        append(
            relation,
            *object_id,
            crate::catalog::sequence::catalog_oids::sequence_catalog_oid(
                &definitions.sequence_catalog_oids,
                object_id,
            ),
        );
    }
    let indexes = if legacy {
        super::super::pg_catalog::legacy_index_relations(catalog, resolution)?
    } else {
        super::super::pg_catalog::catalog_index_relations(catalog, resolution)?
    };
    for index in indexes {
        let oid = index.oid();
        claims.push(RelationClaim {
            relation: index.relation,
            object_id: index
                .definition
                .catalog
                .map(|identity| identity.identity.object_id),
            oid,
        });
    }
    for graph in crate::catalog::graph::graph_catalog_entries(catalog)? {
        // A graph's relations hold the OIDs it recorded, or for a graph or label created before OIDs were recorded the ones their names derive.
        let recorded = catalog.graph_catalog_oids(&graph.name);
        let derived =
            |kind, name: &str| uqa_sql::catalog::oids::relation_oid(kind, &graph.name, name);
        claims.push(RelationClaim {
            relation: RelationIdentity::new(&graph.name, "_label_id_seq"),
            object_id: None,
            oid: recorded.map_or_else(
                || derived("S", "_label_id_seq"),
                |oids| i64::from(oids.label_sequence),
            ),
        });
        for label in &graph.labels {
            let label_oids = recorded.and_then(|oids| oids.labels.get(&label.id));
            let sequence = format!("{}_id_seq", label.name);
            claims.push(RelationClaim {
                relation: RelationIdentity::new(&graph.name, &label.name),
                object_id: None,
                oid: label_oids.map_or_else(
                    || derived("r", &label.name),
                    |oids| i64::from(oids.relation.relation),
                ),
            });
            claims.push(RelationClaim {
                relation: RelationIdentity::new(&graph.name, &sequence),
                object_id: None,
                oid: label_oids
                    .map_or_else(|| derived("S", &sequence), |oids| i64::from(oids.sequence)),
            });
        }
    }
    Ok(claims)
}
