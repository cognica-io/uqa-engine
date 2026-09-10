//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Immutable SQL inputs adapted from the active statement scope.

use crate::catalog::{analysis::CatalogReadView, resolution::RelationNameResolution};
use crate::{
    plan::{CtePlan, QueryPlan},
    RowSchema,
};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone)]
pub struct BindingContext<'a> {
    pub catalog: CatalogReadView,
    pub resolution: RelationNameResolution,
    pub ctes: BTreeMap<String, RowSchema>,
    pub deferred_ctes: BTreeMap<String, CtePlan>,
    pub non_returning_ctes: BTreeSet<String>,
    pub scalar_subqueries: &'a [QueryPlan],
}
