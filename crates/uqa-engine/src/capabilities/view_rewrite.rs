//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bind logical view analysis to current catalog generations and session state.

use crate::Engine;
use std::{collections::BTreeSet, sync::Arc};
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
    fn view_definition(&self, name: &str) -> Result<Option<ViewRewriteDefinition>, SQLError> {
        Ok(Engine::view_definition(self, name)?.map(|definition| definition.rewrite_definition()))
    }
    fn try_resolve_view_name(&self, name: &str) -> Result<Option<String>, String> {
        Engine::try_resolve_view_name(self, name).map_err(|error| error.to_string())
    }
    fn try_describe_table(&self, name: &str) -> Result<Option<Vec<ColumnDef>>, String> {
        Engine::try_describe_table(self, name).map_err(|error| error.to_string())
    }
    fn try_table_columns(&self, name: &str) -> Result<Vec<String>, String> {
        Engine::try_table_columns(self, name).map_err(|error| error.to_string())
    }
    fn rules_for(&self, name: &str, event: RuleEvent) -> Result<Vec<StoredRule>, SQLError> {
        Engine::rules_for(self, name, event)
    }
    fn rule_definitions_for(
        &self,
        name: &str,
        event: RuleEvent,
    ) -> Result<Vec<StoredRule>, SQLError> {
        Engine::rule_definitions_for(self, name, event)
    }
    fn has_trigger_definition(
        &self,
        name: &str,
        timing: TriggerTiming,
        event: TriggerEvent,
        row: bool,
    ) -> Result<bool, SQLError> {
        Engine::has_trigger_definition(self, name, timing, event, row)
    }
    fn rule_new_row_columns(
        &self,
        rule: &StoredRule,
    ) -> Result<Option<BTreeSet<String>>, SQLError> {
        crate::events::rule_new_row_columns(self, rule)
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
            authorization: self,
        }
    }
}
impl ViewPrivilegeCatalog for Engine {
    fn view_definition(&self, name: &str) -> Result<Option<StoredView>, SQLError> {
        self.view_definition(name)
    }
    fn current_user_name(&self) -> String {
        self.current_user_name()
    }
    fn ensure_view_privilege_for(
        &self,
        name: &str,
        view: &StoredView,
        subject: &str,
        privilege: TableAclPrivilege,
    ) -> Result<(), SQLError> {
        self.ensure_view_privilege_for(name, view, subject, privilege)
    }
    fn ensure_view_column_privilege_for(
        &self,
        name: &str,
        view: &StoredView,
        column: &str,
        subject: &str,
        privilege: TableAclPrivilege,
    ) -> Result<(), SQLError> {
        self.ensure_view_column_privilege_for(name, view, column, subject, privilege)
    }
    fn ensure_any_view_column_privilege_for(
        &self,
        name: &str,
        view: &StoredView,
        subject: &str,
        privilege: TableAclPrivilege,
    ) -> Result<(), SQLError> {
        self.ensure_any_view_column_privilege_for(name, view, subject, privilege)
    }
    fn ensure_target_select(
        &self,
        request: TargetSelectPrivilegeRequest<'_, '_>,
    ) -> Result<(), SQLError> {
        let mut scope = super::query_scope::new_for_current_routine(self);
        uqa_execution::query::privileges::ensure_target_table_select_for_expressions(
            request, &mut scope,
        )
    }
}
