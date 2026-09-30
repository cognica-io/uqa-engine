//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! The catalog OIDs graphs recorded for their labels' objects, which occupy the OID space of each object's catalog.

use uqa_sql::catalog::graph_oids::LabelCatalogOids;
use uqa_sql::schema::constraint_metadata::CatalogOidClass;

use crate::catalog::CatalogReadView;

/// Whether a graph's recorded objects hold `oid` in the catalog of `class`.
pub(super) fn graph_oid_in_use(
    catalog: &CatalogReadView,
    class: CatalogOidClass,
    oid: i64,
) -> bool {
    catalog
        .snapshot()
        .definitions
        .graph_catalog_oids
        .values()
        .any(|graph| {
            (class == CatalogOidClass::Relation && i64::from(graph.label_sequence) == oid)
                || graph
                    .labels
                    .values()
                    .any(|label| class_oids(label, class).any(|claimed| i64::from(claimed) == oid))
        })
}

fn class_oids(
    label: &LabelCatalogOids,
    class: CatalogOidClass,
) -> Box<dyn Iterator<Item = u32> + '_> {
    match class {
        CatalogOidClass::Relation => Box::new(
            [
                label.sequence,
                label.relation.relation,
                label.toast_table,
                label.toast_index,
            ]
            .into_iter()
            .chain(label.primary_key.map(|key| key.index))
            .chain(
                label
                    .endpoint_indexes
                    .into_iter()
                    .flat_map(|indexes| [indexes.start_id, indexes.end_id]),
            ),
        ),
        CatalogOidClass::Type => Box::new(
            [label.relation.row_type, label.relation.array_type]
                .into_iter()
                .flatten(),
        ),
        CatalogOidClass::Constraint => Box::new(
            label
                .not_null
                .values()
                .copied()
                .chain(label.primary_key.map(|key| key.constraint)),
        ),
        CatalogOidClass::AttributeDefault => {
            Box::new([label.id_default, label.properties_default].into_iter())
        }
        CatalogOidClass::Trigger => Box::new(std::iter::once(label.trigger)),
        CatalogOidClass::EnumLabel | CatalogOidClass::Rewrite | CatalogOidClass::Procedure => {
            Box::new(std::iter::empty())
        }
    }
}
