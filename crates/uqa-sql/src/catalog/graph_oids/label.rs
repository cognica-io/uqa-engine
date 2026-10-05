//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! The public OIDs of one AGE label's catalog objects.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::catalog::relation_oids::RelationCatalogOids;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LabelCatalogOids {
    /// `<label>_id_seq`.
    pub sequence: u32,
    /// The label's relation with its array and row types.
    pub relation: RelationCatalogOids,
    /// `pg_attrdef` of the `id` column.
    pub id_default: u32,
    /// `pg_attrdef` of the `properties` column.
    pub properties_default: u32,
    /// The NOT NULL constraints by column name.
    pub not_null: BTreeMap<String, u32>,
    /// The TOAST table the `agtype` properties column needs, and its index.
    pub toast_table: u32,
    pub toast_index: u32,
    /// The primary key's index and constraint: every vertex label and the default edge label have one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub primary_key: Option<IndexConstraintOids>,
    /// The `start_id` and `end_id` indexes of an edge label.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub endpoint_indexes: Option<EndpointIndexOids>,
    /// The `_age_cache_invalidate` trigger.
    pub trigger: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IndexConstraintOids {
    pub index: u32,
    pub constraint: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EndpointIndexOids {
    pub start_id: u32,
    pub end_id: u32,
}

impl LabelCatalogOids {
    /// Every OID the label's objects claim, for occupancy checks.
    pub fn claimed(&self) -> impl Iterator<Item = u32> + '_ {
        [
            self.sequence,
            self.id_default,
            self.properties_default,
            self.toast_table,
            self.toast_index,
            self.trigger,
        ]
        .into_iter()
        .chain(self.relation.claimed())
        .chain(self.not_null.values().copied())
        .chain(
            self.primary_key
                .iter()
                .flat_map(|key| [key.index, key.constraint]),
        )
        .chain(
            self.endpoint_indexes
                .iter()
                .flat_map(|indexes| [indexes.start_id, indexes.end_id]),
        )
    }
}
