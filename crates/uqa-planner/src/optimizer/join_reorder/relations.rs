//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Native join metadata borrows source names and retains captured statistics in the bounded relation domain.

use crate::{join_order::JoinRelationRef, AccessParadigm, ColumnStatistics};
use std::borrow::Cow;

#[derive(Default)]
pub(super) struct RelationMetadata<'a> {
    pub alias: Cow<'a, str>,
    pub cardinality: f64,
    pub column_stats: ColumnStatistics,
    pub access_cost: f64,
    pub paradigm: AccessParadigm,
}

impl RelationMetadata<'_> {
    pub(super) fn as_join_relation(&self) -> JoinRelationRef<'_> {
        JoinRelationRef {
            alias: &self.alias,
            cardinality: self.cardinality,
            column_stats: &self.column_stats,
            access_cost: self.access_cost,
        }
    }
}
