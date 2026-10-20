//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Immutable relation authorization and CTE name inputs for column lineage analysis.

use crate::catalog::roles::RoleReference;
use crate::{
    catalog::resolution::RelationNameResolution,
    plan::{CtePlan, QueryPlan},
    SQLError,
};
use std::borrow::Cow;
use std::collections::BTreeMap;

#[derive(Clone, Copy)]
pub enum PrivilegeRelationKind {
    System(crate::catalog::SystemRelation),
    Table,
    View,
    MaterializedView,
    ForeignTable,
}
impl PrivilegeRelationKind {
    pub const fn has_system_columns(self) -> bool {
        match self {
            Self::System(relation) => matches!(relation.kind().as_bytes(), b"table"),
            _ => matches!(self, Self::Table | Self::ForeignTable),
        }
    }
    pub const fn description(self) -> &'static str {
        match self {
            Self::System(relation) => relation.kind(),
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
        subject: &RoleReference,
    ) -> Result<bool, SQLError>;
}
pub trait PrivilegeCteCatalog {
    fn is_visible_cte(&self, name: &str) -> bool;
    fn materialized_columns(&self, name: &str) -> Option<Vec<String>>;
    fn deferred_reference(&self, name: &str) -> Option<&CtePlan>;
    fn privilege_subject(&self) -> Result<&RoleReference, SQLError>;
}
#[derive(Clone)]
pub struct PrivilegeScope<'a, Queries = Vec<QueryPlan>> {
    pub catalog: &'a dyn PrivilegeCatalog,
    pub resolution: RelationNameResolution,
    pub inherited: &'a dyn PrivilegeCteCatalog,
    pub scalar_subqueries: Queries,
    local_ctes: BTreeMap<String, Cow<'a, CtePlan>>,
    pub(super) include_authorization_only_columns: bool,
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
            include_authorization_only_columns: true,
        }
    }
}

impl<'a> PrivilegeScope<'a, &'a [QueryPlan]> {
    /// Borrow the executing statement's immutable query arena without copying its plans. The owned constructor and its public vector remain available for callers that retain a scope.
    pub fn borrowed(
        catalog: &'a dyn PrivilegeCatalog,
        resolution: RelationNameResolution,
        inherited: &'a dyn PrivilegeCteCatalog,
        scalar_subqueries: &'a [QueryPlan],
    ) -> Self {
        Self {
            catalog,
            resolution,
            inherited,
            scalar_subqueries,
            local_ctes: BTreeMap::new(),
            include_authorization_only_columns: true,
        }
    }
}

impl<'a, Queries: AsRef<[QueryPlan]>> PrivilegeScope<'a, Queries> {
    pub fn insert_deferred(&mut self, plan: CtePlan) {
        self.local_ctes.insert(plan.name.clone(), Cow::Owned(plan));
    }

    pub(super) fn insert_deferred_ref(&mut self, plan: &'a CtePlan) {
        self.local_ctes
            .insert(plan.name.clone(), Cow::Borrowed(plan));
    }

    pub(super) fn reborrow(&self) -> PrivilegeScope<'_, &[QueryPlan]> {
        PrivilegeScope {
            catalog: self.catalog,
            resolution: self.resolution.clone(),
            inherited: self.inherited,
            scalar_subqueries: self.scalar_subqueries.as_ref(),
            local_ctes: self
                .local_ctes
                .iter()
                .map(|(name, plan)| (name.clone(), Cow::Borrowed(plan.as_ref())))
                .collect(),
            include_authorization_only_columns: self.include_authorization_only_columns,
        }
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
            .map(AsRef::as_ref)
            .or_else(|| self.inherited.deferred_reference(name))
    }

    pub fn privilege_subject(&self) -> Result<&RoleReference, SQLError> {
        self.inherited.privilege_subject()
    }
}
