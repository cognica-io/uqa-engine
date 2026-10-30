//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bind logical view analysis to current catalog generations and session state.

use crate::Engine;
use std::{collections::BTreeSet, sync::Arc};
use uqa_core::memory::{Produced, ProductionControl};
use uqa_sql::catalog::roles::RoleReference;
use uqa_sql::{
    ast::{ColumnDef, RuleEvent, TriggerEvent, TriggerTiming},
    binding::snapshot::BindingSnapshot,
    catalog::{
        analysis::CatalogReadView,
        events::StoredRule,
        resolution::RelationNameResolution,
        view::{StoredViewKind, ViewRewriteDefinition},
    },
    semantics::view_rewrite::context::ViewRewriteCatalog,
    SQLError,
};

impl ViewRewriteCatalog for Engine {
    fn visit_table_column_names_with_control(
        &self,
        name: &str,
        control: &ProductionControl<'_>,
        read: &mut dyn FnMut(&str) -> Result<(), SQLError>,
    ) -> Result<(), SQLError> {
        if uqa_sql::semantics::returning::ReturningCatalog::with_table_row_type_with_control(
            self,
            name,
            control,
            &mut |columns| columns.iter().try_for_each(|column| read(&column.name)),
        )? {
            Ok(())
        } else {
            Err(SQLError::UnknownTable(
                control.copy_text(name)?.into_parts().0,
            ))
        }
    }

    fn capture_rules(
        &self,
        name: &str,
        event: RuleEvent,
    ) -> Result<uqa_sql::catalog::events::selection::CapturedRules, SQLError> {
        uqa_sql::semantics::rules::RuleCatalog::capture_rules(self, name, event)
    }
    fn capture_rules_with_control(
        &self,
        name: &str,
        event: RuleEvent,
        control: &ProductionControl<'_>,
    ) -> Result<uqa_sql::catalog::events::selection::CapturedRules, SQLError> {
        uqa_sql::semantics::rules::RuleCatalog::capture_rules_with_control(
            self, name, event, control,
        )
    }
    fn with_view_definition_with_control(
        &self,
        name: &str,
        control: &uqa_core::memory::ProductionControl<'_>,
        read: &mut dyn FnMut(
            Option<uqa_sql::catalog::view::ViewRewriteDefinitionRef<'_>>,
        ) -> Result<(), SQLError>,
    ) -> Result<(), SQLError> {
        if Engine::with_view_definition_with_control(self, name, control, |_, view| {
            read(Some(view.rewrite_definition_ref()))
        })?
        .is_none()
        {
            read(None)?;
        }
        Ok(())
    }
    fn try_resolve_view_name_with_control(
        &self,
        name: &str,
        control: &uqa_core::memory::ProductionControl<'_>,
    ) -> Result<Option<uqa_core::memory::Produced<String>>, SQLError> {
        Engine::try_resolve_view_name_with_control(self, name, control)
    }
    fn visit_rules_with_control(
        &self,
        name: &str,
        event: RuleEvent,
        reverse: bool,
        control: &uqa_core::memory::ProductionControl<'_>,
        visit: &mut dyn FnMut(&StoredRule) -> Result<(), SQLError>,
    ) -> Result<(), SQLError> {
        uqa_sql::semantics::rules::RuleCatalog::visit_rules_with_control(
            self, name, event, reverse, control, visit,
        )
    }
    fn target_view_kind_with_control(
        &self,
        name: &str,
        control: &uqa_core::memory::ProductionControl<'_>,
    ) -> Result<Option<StoredViewKind>, SQLError> {
        self.mutation_view_kind_with_control(name, control)
    }
    fn has_trigger_definition_with_control(
        &self,
        name: &str,
        timing: TriggerTiming,
        event: TriggerEvent,
        row: bool,
        control: &uqa_core::memory::ProductionControl<'_>,
    ) -> Result<bool, SQLError> {
        let relation = self
            .event_analysis_context()
            .resolve_trigger_table_with_control(name, control)?;
        let triggers = self.query_catalog_snapshot.as_ref().map_or_else(
            || self.durable.triggers.snapshot(),
            |snapshot| snapshot.triggers.clone(),
        );
        uqa_sql::catalog::events::selection::has_trigger_definition_with_control(
            &triggers, &relation, timing, event, row, control,
        )
    }
    fn binding_scope_with_control(
        &self,
        control: &uqa_core::memory::ProductionControl<'_>,
    ) -> Result<uqa_sql::binding::production::ControlledBindingSnapshot, SQLError> {
        let scope =
            crate::capabilities::query_scope::new_for_current_routine_with_control(self, control)?;
        uqa_execution::query::binding::binding_snapshot_with_control(&scope, control)
    }
    fn restored_view_scope_with_control(
        &self,
        control: &uqa_core::memory::ProductionControl<'_>,
    ) -> Result<uqa_sql::binding::production::ControlledBindingContext<'static>, SQLError> {
        use uqa_sql::binding::production::{
            BindingMap, BindingNameSet, BindingResolution, ControlledBindingContext,
        };
        let catalog = control
            .finish(
                self.restored_catalog_read_view_with_control(control)?,
                control.empty_reservation(),
            )?
            .into_shared(control)?;
        let resolution = uqa_execution::catalog::services::CatalogSession::relation_name_resolution_with_control(self, control)?;
        Ok(ControlledBindingContext {
            catalog,
            resolution: BindingResolution::from_produced(resolution),
            ctes: BindingMap::new(),
            deferred_ctes: BindingMap::new(),
            non_returning_ctes: BindingNameSet::new(),
            scalar_subqueries: &[],
        })
    }
    fn view_definition(&self, name: &str) -> Result<Option<ViewRewriteDefinition>, SQLError> {
        Ok(Engine::view_definition(self, name)?.map(|definition| definition.rewrite_definition()))
    }
    fn try_resolve_view_name(&self, name: &str) -> Result<Option<String>, String> {
        Engine::try_resolve_view_name(self, name).map_err(|error| error.to_string())
    }
    fn try_describe_table(&self, name: &str) -> Result<Option<Vec<ColumnDef>>, String> {
        Engine::describe_table_in_execution(self, name).map_err(|error| error.to_string())
    }
    fn try_table_columns(&self, name: &str) -> Result<Vec<String>, String> {
        Engine::table_columns_in_execution(self, name).map_err(|error| error.to_string())
    }
    fn rules_for(&self, name: &str, event: RuleEvent) -> Result<Vec<StoredRule>, SQLError> {
        self.event_lookup_context().rules_for(name, event)
    }
    fn rule_definitions_for(
        &self,
        name: &str,
        event: RuleEvent,
    ) -> Result<Vec<StoredRule>, SQLError> {
        self.event_lookup_context()
            .rule_definitions_for(name, event)
    }
    fn has_trigger_definition(
        &self,
        name: &str,
        timing: TriggerTiming,
        event: TriggerEvent,
        row: bool,
    ) -> Result<bool, SQLError> {
        self.event_lookup_context()
            .has_trigger_definition(name, timing, event, row)
    }
    fn rule_new_row_columns(
        &self,
        rule: &StoredRule,
    ) -> Result<Option<BTreeSet<String>>, SQLError> {
        uqa_sql::semantics::rules::action_binding::rule_new_row_columns(self, rule)
    }
    fn rule_new_row_columns_with_control(
        &self,
        rule: &StoredRule,
        control: &ProductionControl<'_>,
    ) -> Result<Option<Produced<Vec<String>>>, SQLError> {
        uqa_sql::semantics::rules::action_binding::rule_new_row_columns_with_control(
            self, rule, control,
        )
    }
    fn target_view_kind(&self, name: &str) -> Result<Option<StoredViewKind>, SQLError> {
        self.mutation_view_kind(name)
    }
    fn binding_scope(&self) -> Result<BindingSnapshot, SQLError> {
        let scope = crate::capabilities::query_scope::new_for_current_routine(self);
        uqa_execution::query::binding::binding_context(&scope).map(BindingSnapshot::from)
    }
    fn restored_view_catalog(&self) -> (CatalogReadView, RelationNameResolution) {
        (
            Arc::new(self.restored_catalog_read_view()),
            self.session_execution_view().relation_name_resolution(),
        )
    }
}

use uqa_sql::{
    catalog::{security::table::TableAclPrivilege, stored_view::StoredView},
    semantics::{
        privileges::TargetSelectPrivilegeRequest, view_privileges::ViewPrivilegeCatalog,
        view_rewrite::context::ViewRewriteContext,
    },
};
impl Engine {
    pub(crate) fn view_rewrite_context(&self) -> ViewRewriteContext<'_> {
        ViewRewriteContext {
            catalog: self,
            routines: self,
            authorization: self,
        }
    }
}
impl ViewPrivilegeCatalog for Engine {
    fn with_view_definition_with_control(
        &self,
        name: &str,
        control: &uqa_core::memory::ProductionControl<'_>,
        read: &mut dyn FnMut(Option<&StoredView>) -> Result<(), SQLError>,
    ) -> Result<(), SQLError> {
        if Engine::with_view_definition_with_control(self, name, control, |_, view| {
            read(Some(view))
        })?
        .is_none()
        {
            read(None)?;
        }
        Ok(())
    }
    fn ensure_query_select_with_control(
        &self,
        query: &uqa_sql::plan::QueryPlan,
        control: &uqa_core::memory::ProductionControl<'_>,
    ) -> Result<(), SQLError> {
        let scope = super::query_scope::new_for_current_routine_with_control(self, control)?;
        uqa_execution::query::privileges::ensure_select_privileges_for_query_with_control(
            query, &scope, control,
        )
    }
    fn bound_role_with_control(
        &self,
        identity: uqa_sql::catalog::roles::RoleIdentity,
        control: &uqa_core::memory::ProductionControl<'_>,
    ) -> Result<uqa_core::memory::Produced<RoleReference>, SQLError> {
        let role = RoleReference::from_identity_with_control(
            identity,
            &self.durable.roles.read(),
            control,
        )?;
        Ok(control.finish(role, control.empty_reservation())?)
    }
    fn ensure_query_select(&self, query: &uqa_sql::plan::QueryPlan) -> Result<(), SQLError> {
        let scope = super::query_scope::new_for_current_routine(self);
        uqa_execution::query::privileges::ensure_select_privileges_for_query(query, &scope)
    }
    fn view_definition(&self, name: &str) -> Result<Option<StoredView>, SQLError> {
        self.view_definition(name)
    }
    fn current_role(&self) -> RoleReference {
        self.current_role()
    }
    fn bound_role(
        &self,
        identity: uqa_sql::catalog::roles::RoleIdentity,
    ) -> Result<RoleReference, SQLError> {
        RoleReference::from_identity(identity, &self.durable.roles.read())
    }
    fn ensure_view_privilege_for_with_control(
        &self,
        name: &str,
        view: &StoredView,
        subject: &RoleReference,
        privilege: TableAclPrivilege,
        control: &uqa_core::memory::ProductionControl<'_>,
    ) -> Result<(), SQLError> {
        uqa_sql::catalog::security::view_authorization::ViewAuthorizationContext { roles: self }
            .ensure_view_privilege_for_with_control(name, view, subject, privilege, control)
    }
    fn ensure_view_column_privilege_for_with_control(
        &self,
        name: &str,
        view: &StoredView,
        column: &str,
        subject: &RoleReference,
        privilege: TableAclPrivilege,
        control: &uqa_core::memory::ProductionControl<'_>,
    ) -> Result<(), SQLError> {
        uqa_sql::catalog::security::view_authorization::ViewAuthorizationContext { roles: self }
            .ensure_view_column_privilege_for_with_control(
                name, view, column, subject, privilege, control,
            )
    }
    fn ensure_any_view_column_privilege_for_with_control(
        &self,
        name: &str,
        view: &StoredView,
        subject: &RoleReference,
        privilege: TableAclPrivilege,
        control: &uqa_core::memory::ProductionControl<'_>,
    ) -> Result<(), SQLError> {
        uqa_sql::catalog::security::view_authorization::ViewAuthorizationContext { roles: self }
            .ensure_any_view_column_privilege_for_with_control(
                name, view, subject, privilege, control,
            )
    }
    fn ensure_view_privilege_for(
        &self,
        name: &str,
        view: &StoredView,
        subject: &RoleReference,
        privilege: TableAclPrivilege,
    ) -> Result<(), SQLError> {
        self.ensure_view_privilege_for(name, view, subject, privilege)
    }
    fn ensure_view_column_privilege_for(
        &self,
        name: &str,
        view: &StoredView,
        column: &str,
        subject: &RoleReference,
        privilege: TableAclPrivilege,
    ) -> Result<(), SQLError> {
        self.ensure_view_column_privilege_for(name, view, column, subject, privilege)
    }
    fn ensure_any_view_column_privilege_for(
        &self,
        name: &str,
        view: &StoredView,
        subject: &RoleReference,
        privilege: TableAclPrivilege,
    ) -> Result<(), SQLError> {
        self.ensure_any_view_column_privilege_for(name, view, subject, privilege)
    }
    fn ensure_target_select(
        &self,
        request: TargetSelectPrivilegeRequest<'_, '_>,
    ) -> Result<(), SQLError> {
        self.ensure_target_select_with_control(
            request,
            &uqa_core::memory::ProductionControl::uncontrolled(),
        )
    }
    fn ensure_target_select_with_control(
        &self,
        request: TargetSelectPrivilegeRequest<'_, '_>,
        control: &uqa_core::memory::ProductionControl<'_>,
    ) -> Result<(), SQLError> {
        let scope = super::query_scope::new_for_current_routine_with_control(self, control)?;
        uqa_execution::query::privileges::ensure_target_table_select_for_expressions_with_control(
            request, &scope, control,
        )
    }
}
