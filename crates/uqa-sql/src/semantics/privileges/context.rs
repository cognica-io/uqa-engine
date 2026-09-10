//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Immutable relation authorization and CTE name inputs for column lineage analysis.

use crate::{
    catalog::resolution::RelationNameResolution,
    plan::{CtePlan, QueryPlan},
    SQLError,
};
use std::collections::BTreeMap;

#[derive(Clone, Copy)]
pub enum PrivilegeRelationKind {
    Table,
    View,
    MaterializedView,
    ForeignTable,
}
impl PrivilegeRelationKind {
    pub const fn has_system_columns(self) -> bool {
        matches!(self, Self::Table | Self::ForeignTable)
    }
    pub const fn description(self) -> &'static str {
        match self {
            Self::Table => "table",
            Self::View => "view",
            Self::MaterializedView => "materialized view",
            Self::ForeignTable => "foreign table",
        }
    }
}
pub struct PrivilegeRelation {
    pub canonical: String,
    pub columns: Vec<String>,
    pub kind: PrivilegeRelationKind,
}
pub trait PrivilegeCatalog {
    fn relation(
        &self,
        resolution: &RelationNameResolution,
        name: &str,
    ) -> Result<Option<PrivilegeRelation>, SQLError>;
    fn has_select_privilege(
        &self,
        resolution: &RelationNameResolution,
        relation: &PrivilegeRelation,
        column: Option<&str>,
        subject: &str,
    ) -> Result<bool, SQLError>;
}
pub trait PrivilegeCteCatalog {
    fn is_visible_cte(&self, name: &str) -> bool;
    fn materialized_columns(&self, name: &str) -> Option<Vec<String>>;
    fn deferred_reference(&self, name: &str) -> Option<&CtePlan>;
    fn privilege_subject(&self) -> Result<&str, SQLError>;
}
#[derive(Clone)]
pub struct PrivilegeScope<'a> {
    pub catalog: &'a dyn PrivilegeCatalog,
    pub resolution: RelationNameResolution,
    pub inherited: &'a dyn PrivilegeCteCatalog,
    pub scalar_subqueries: Vec<QueryPlan>,
    local_ctes: BTreeMap<String, CtePlan>,
}
impl<'a> PrivilegeScope<'a> {
    pub fn new(
        catalog: &'a dyn PrivilegeCatalog,
        resolution: RelationNameResolution,
        inherited: &'a dyn PrivilegeCteCatalog,
        scalar_subqueries: Vec<QueryPlan>,
    ) -> Self {
        Self {
            catalog,
            resolution,
            inherited,
            scalar_subqueries,
            local_ctes: BTreeMap::new(),
        }
    }
    pub fn insert_deferred(&mut self, plan: CtePlan) {
        self.local_ctes.insert(plan.name.clone(), plan);
    }
    pub fn is_visible_cte(&self, name: &str) -> bool {
        crate::semantics::cte_reference_name(name)
            .is_some_and(|name| self.local_ctes.contains_key(&name))
            || self.inherited.is_visible_cte(name)
    }
    pub fn materialized_for_scan(&self, name: &str) -> Option<Vec<String>> {
        let canonical = crate::semantics::cte_reference_name(name)?;
        if self.local_ctes.contains_key(&canonical) {
            None
        } else {
            self.inherited.materialized_columns(name)
        }
    }
    pub fn deferred_reference(&self, name: &str) -> Option<&CtePlan> {
        self.local_ctes
            .get(&crate::semantics::cte_reference_name(name)?)
            .or_else(|| self.inherited.deferred_reference(name))
    }

    pub fn privilege_subject(&self) -> Result<&str, SQLError> {
        self.inherited.privilege_subject()
    }
}
