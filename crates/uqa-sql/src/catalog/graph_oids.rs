//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! The public OIDs of an Apache AGE graph's catalog objects, allocated from the database's counter in the order AGE creates them: the graph's schema, whose OID `ag_graph.graphid` repeats, its label id sequence, and for each label its id sequence, its relation with the array and row types, the defaults and NOT NULL constraints of its columns, its TOAST table and index, its key and endpoint indexes and its cache-invalidation trigger. Graphs and labels created before OIDs were recorded derive theirs from their names.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

mod label;

pub use label::{EndpointIndexOids, IndexConstraintOids, LabelCatalogOids};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GraphCatalogOids {
    /// `pg_namespace.oid` of the graph's schema, which is also its `ag_graph.graphid`.
    pub namespace: u32,
    /// `_label_id_seq`.
    pub label_sequence: u32,
    /// Each label's OIDs by its AGE label id.
    pub labels: BTreeMap<u32, LabelCatalogOids>,
}

impl GraphCatalogOids {
    /// Every OID the graph's objects claim, for occupancy checks.
    pub fn claimed(&self) -> impl Iterator<Item = u32> + '_ {
        [self.namespace, self.label_sequence]
            .into_iter()
            .chain(self.labels.values().flat_map(LabelCatalogOids::claimed))
    }
}
