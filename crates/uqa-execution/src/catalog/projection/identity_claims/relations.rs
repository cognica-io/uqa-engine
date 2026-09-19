//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Relation-class address claims from unfiltered definitions, without table statistics or row scans.

use crate::catalog::{CatalogReadView, RelationNameResolution};
use uqa_core::RelationIdentity;
use uqa_sql::{catalog::oids::stable_object_oid, SQLError};

pub(crate) struct RelationClaim {
    pub relation: RelationIdentity,
    pub object_id: Option<[u8; 16]>,
    pub oid: i64,
}

pub(crate) fn relation_claims(
    catalog: &CatalogReadView,
    resolution: &RelationNameResolution,
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
            stable_object_oid("relation", &table.object_id),
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
            stable_object_oid("relation", object_id),
        );
    }
    for index in super::super::pg_catalog::catalog_index_relations(catalog, resolution)? {
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
        for (kind, name) in std::iter::once(("S", "_label_id_seq".to_string())).chain(
            graph.labels.iter().flat_map(|label| {
                [
                    ("r", label.name.clone()),
                    ("S", format!("{}_id_seq", label.name)),
                ]
            }),
        ) {
            claims.push(RelationClaim {
                relation: RelationIdentity::new(&graph.name, &name),
                object_id: None,
                oid: uqa_sql::catalog::oids::relation_oid(kind, &graph.name, &name),
            });
        }
    }
    Ok(claims)
}
