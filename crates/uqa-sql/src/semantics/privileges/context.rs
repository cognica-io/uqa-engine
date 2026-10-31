//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Immutable relation authorization and CTE name inputs for column lineage analysis.

use super::ownership::{self, Named};
use crate::binding::production::BindingResolution;
use crate::catalog::roles::RoleReference;
use crate::{
    catalog::resolution::RelationNameResolution,
    plan::{CtePlan, QueryPlan},
    SQLError,
};
use std::borrow::Cow;
use uqa_core::memory::{OwnedProductionControl, Produced, ProductionControl, ProductionVec};

#[derive(Clone, Copy, Debug)]
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
#[derive(Debug)]
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
    fn relation_with_control(
        &self,
        resolution: &RelationNameResolution,
        name: &str,
        control: &ProductionControl<'_>,
    ) -> Result<Option<Produced<PrivilegeRelation>>, SQLError> {
        control.check()?;
        self.relation(resolution, name)?
            .map(|relation| {
                PrivilegeRelation::from_names(
                    &relation.canonical,
                    relation.columns.iter().map(String::as_str),
                    relation.kind,
                    control,
                )
            })
            .transpose()
    }
    fn has_select_privilege_with_control(
        &self,
        resolution: &RelationNameResolution,
        relation: &PrivilegeRelation,
        column: Option<&str>,
        subject: &RoleReference,
        control: &ProductionControl<'_>,
    ) -> Result<bool, SQLError> {
        control.check()?;
        self.has_select_privilege(resolution, relation, column, subject)
    }
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
    fn is_visible_cte_with_control(
        &self,
        name: &str,
        control: &ProductionControl<'_>,
    ) -> Result<bool, SQLError> {
        control.check()?;
        Ok(self.is_visible_cte(name))
    }
    fn materialized_columns(&self, name: &str) -> Option<Vec<String>>;
    fn materialized_columns_with_control(
        &self,
        name: &str,
        control: &ProductionControl<'_>,
    ) -> Result<Option<Produced<Vec<String>>>, SQLError> {
        control.check()?;
        self.materialized_columns(name)
            .map(|columns| copy_columns(columns.iter().map(String::as_str), control))
            .transpose()
    }
    fn deferred_reference(&self, name: &str) -> Option<&CtePlan>;
    fn deferred_reference_with_control(
        &self,
        name: &str,
        control: &ProductionControl<'_>,
    ) -> Result<Option<&CtePlan>, SQLError> {
        control.check()?;
        Ok(self.deferred_reference(name))
    }
    fn privilege_subject(&self) -> Result<&RoleReference, SQLError>;
}
pub struct PrivilegeScope<'a, Queries = Vec<QueryPlan>> {
    pub catalog: &'a dyn PrivilegeCatalog,
    pub resolution: BindingResolution,
    pub inherited: &'a dyn PrivilegeCteCatalog,
    pub scalar_subqueries: Queries,
    local_ctes: Named<Cow<'a, CtePlan>>,
    control: OwnedProductionControl,
    pub(super) include_authorization_only_columns: bool,
    pub(super) relation_privilege_subject: Option<Produced<RoleReference>>,
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
            resolution: resolution.into(),
            inherited,
            scalar_subqueries,
            local_ctes: Named::new(),
            control: ProductionControl::uncontrolled().capture(),
            include_authorization_only_columns: true,
            relation_privilege_subject: None,
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
            resolution: resolution.into(),
            inherited,
            scalar_subqueries,
            local_ctes: Named::new(),
            control: ProductionControl::uncontrolled().capture(),
            include_authorization_only_columns: true,
            relation_privilege_subject: None,
        }
    }
}

impl<'a> PrivilegeScope<'a, &'a [QueryPlan]> {
    pub fn borrowed_with_control(
        catalog: &'a dyn PrivilegeCatalog,
        resolution: Produced<RelationNameResolution>,
        inherited: &'a dyn PrivilegeCteCatalog,
        scalar_subqueries: &'a [QueryPlan],
        control: &ProductionControl<'_>,
    ) -> Self {
        Self {
            catalog,
            resolution: BindingResolution::from_produced(resolution),
            inherited,
            scalar_subqueries,
            local_ctes: Named::new(),
            control: control.capture(),
            include_authorization_only_columns: true,
            relation_privilege_subject: None,
        }
    }
}

impl<'a, Queries: AsRef<[QueryPlan]>> PrivilegeScope<'a, Queries> {
    pub(super) fn control(&self) -> ProductionControl<'_> {
        self.control.control()
    }

    pub fn insert_deferred(&mut self, plan: CtePlan) -> Result<(), SQLError> {
        let control = self.control.clone();
        let name = control.control().copy_text(&plan.name)?;
        let value = Cow::Owned(plan);
        match control.control().budget() {
            Some(budget) => {
                self.local_ctes.insert_budgeted(name, value, budget)?;
            }
            None => {
                self.local_ctes.insert(name, value);
            }
        }
        Ok(())
    }

    pub(super) fn insert_deferred_ref(&mut self, plan: &'a CtePlan) -> Result<(), SQLError> {
        ownership::insert(
            &mut self.local_ctes,
            &plan.name,
            Cow::Borrowed(plan),
            &self.control.control(),
        )
    }

    pub(super) fn reborrow(&self) -> Result<PrivilegeScope<'_, &[QueryPlan]>, SQLError> {
        let control = self.control();
        let mut local_ctes = Named::new();
        for (name, plan) in &self.local_ctes {
            ownership::insert(
                &mut local_ctes,
                name,
                Cow::Borrowed(plan.as_ref()),
                &control,
            )?;
        }
        Ok(PrivilegeScope {
            catalog: self.catalog,
            resolution: self.resolution.copy_with_control(&control)?,
            inherited: self.inherited,
            scalar_subqueries: self.scalar_subqueries.as_ref(),
            local_ctes,
            control: self.control.clone(),
            include_authorization_only_columns: self.include_authorization_only_columns,
            relation_privilege_subject: self
                .relation_privilege_subject
                .as_ref()
                .map(|role| role.clone_with_control(&control))
                .transpose()?,
        })
    }
    fn local(&self, reference: &str) -> Option<&CtePlan> {
        let reference = uqa_core::RelationIdentity::borrowed_reference(reference)?;
        if reference.schema.is_some() {
            return None;
        }
        self.local_ctes
            .get_by(|name| reference.name.cmp(&uqa_core::RelationName::decoded(name)))
            .map(AsRef::as_ref)
    }
    pub fn is_visible_cte_with_control(&self, name: &str) -> Result<bool, SQLError> {
        self.control().check()?;
        if self.local(name).is_some() {
            Ok(true)
        } else {
            self.inherited
                .is_visible_cte_with_control(name, &self.control())
        }
    }
    pub fn materialized_for_scan(
        &self,
        name: &str,
    ) -> Result<Option<Produced<Vec<String>>>, SQLError> {
        let Some(reference) = uqa_core::RelationIdentity::borrowed_reference(name) else {
            return Ok(None);
        };
        if reference.schema.is_some() || self.local(name).is_some() {
            Ok(None)
        } else {
            self.inherited
                .materialized_columns_with_control(name, &self.control())
        }
    }
    pub fn deferred_reference_with_control(
        &self,
        name: &str,
    ) -> Result<Option<&CtePlan>, SQLError> {
        self.control().check()?;
        if uqa_core::RelationIdentity::borrowed_reference(name)
            .is_none_or(|reference| reference.schema.is_some())
        {
            return Ok(None);
        }
        if let Some(plan) = self.local(name) {
            Ok(Some(plan))
        } else {
            self.inherited
                .deferred_reference_with_control(name, &self.control())
        }
    }
    pub fn privilege_subject(&self) -> Result<&RoleReference, SQLError> {
        self.relation_privilege_subject
            .as_deref()
            .map_or_else(|| self.inherited.privilege_subject(), Ok)
    }
}

pub(super) fn copy_columns<'a>(
    columns: impl IntoIterator<Item = &'a str>,
    control: &ProductionControl<'_>,
) -> Result<Produced<Vec<String>>, SQLError> {
    let mut output = ProductionVec::new(*control);
    for column in columns {
        output.push_produced(control.copy_text(column)?)?;
    }
    Ok(output.finish()?)
}

impl PrivilegeRelation {
    pub fn from_names<'a>(
        canonical: &str,
        columns: impl IntoIterator<Item = &'a str>,
        kind: PrivilegeRelationKind,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<Self>, SQLError> {
        let canonical = control.copy_text(canonical)?;
        let columns = copy_columns(columns, control)?;
        Self::from_columns(canonical, columns, kind, control)
    }

    pub fn from_columns(
        canonical: Produced<String>,
        columns: Produced<Vec<String>>,
        kind: PrivilegeRelationKind,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<Self>, SQLError> {
        let (canonical, name_memory) = canonical.into_parts();
        let (columns, column_memory) = columns.into_parts();
        Ok(control.finish(
            Self {
                canonical,
                columns,
                kind,
            },
            control.combine(name_memory, column_memory),
        )?)
    }
}
