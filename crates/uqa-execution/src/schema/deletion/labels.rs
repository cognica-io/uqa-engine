//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Dependents of a graph's label relations, which are not catalog objects: the views and routines that name them are found in their stored definitions and removed with what depends on them, as dropping a graph's schema removes them.

use super::{perform_quiet_cascade, required_address, CatalogRemovalContext};
use std::collections::{BTreeMap, BTreeSet};
use uqa_core::RelationIdentity;
use uqa_sql::catalog::stored_view::dependencies::views_depending_on_relation;
use uqa_sql::routines::lifecycle::relations::{
    stored_routine_references_relations, RoutineRelationOids,
};
use uqa_sql::SQLError;

/// Remove the views and routines that name a label relation of `graph`, and what depends on them, without a notice.
pub fn drop_graph_label_dependents(
    context: &CatalogRemovalContext<'_>,
    graph: &str,
    labels: &[String],
) -> Result<(), SQLError> {
    let relations = labels
        .iter()
        .map(|label| RelationIdentity::new(graph, label))
        .collect::<Vec<_>>();
    let catalog = context.catalog.catalog_read_view();
    let oids = LabelRelationOids(
        labels
            .iter()
            .zip(&relations)
            .map(|(label, relation)| {
                Ok((
                    relation.qualified_name(),
                    crate::catalog::projection::named_label_relation_oid(&catalog, graph, label)?,
                ))
            })
            .collect::<Result<_, SQLError>>()?,
    );
    drop(catalog);
    let names = oids.0.keys().cloned().collect::<BTreeSet<_>>();
    perform_quiet_cascade(context, |dependencies| {
        let catalog = context.catalog.catalog_read_view();
        let mut originals = BTreeSet::new();
        for relation in &relations {
            for view in views_depending_on_relation(&catalog.snapshot().definitions.views, relation)
            {
                let identity =
                    RelationIdentity::from_legacy_name(&view).map_err(SQLError::Internal)?;
                originals.insert(required_address(
                    dependencies.relation_address(&identity, None),
                    || format!("view {view}"),
                )?);
            }
        }
        for function in catalog.all_sql_functions() {
            if stored_routine_references_relations(&oids, &function.def, &names)? {
                originals.insert(required_address(
                    function
                        .def
                        .object_id
                        .and_then(|object_id| dependencies.routine_address(&object_id)),
                    || format!("routine {}", function.def.name),
                )?);
            }
        }
        Ok(originals.into_iter().collect())
    })
}

/// The `regclass` OIDs of the label relations, which routine parameter defaults name.
struct LabelRelationOids(BTreeMap<String, i64>);

impl RoutineRelationOids for LabelRelationOids {
    fn bound_regclass_oid(&self, name: &str) -> Result<Option<i64>, SQLError> {
        Ok(self.0.get(name).copied())
    }
}
