//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bind routine namespace lookup to live session and schema authorization state.

use crate::{schema_security::SchemaAclPrivilege, Engine};
use uqa_sql::catalog::roles::RoleReference;
use uqa_sql::{
    catalog::security::BoundSchemaSecurity, routines::lifecycle::names::RoutineNameCatalog,
    SQLError,
};

impl RoutineNameCatalog for Engine {
    fn schema_security(&self, schema: &str) -> Option<BoundSchemaSecurity> {
        self.schema_security_for_privilege(schema)
    }
    fn current_role(&self) -> RoleReference {
        Engine::current_role(self)
    }
    fn search_path(&self) -> Vec<String> {
        crate::session::effective_search_path(&self.session.state.read())
    }
    fn search_path_with_control(
        &self,
        control: &uqa_core::memory::ProductionControl<'_>,
    ) -> Result<uqa_core::memory::Produced<Vec<String>>, SQLError> {
        control.check()?;
        let state = self.session.state.read();
        let role = state.authorization.current();
        let mut path = uqa_core::memory::ProductionVec::new(*control);
        path.reserve(state.search_path.len())?;
        for entry in &state.search_path {
            path.push_produced(control.copy_text(if entry == "$user" {
                &role.name
            } else {
                entry
            })?)?;
        }
        Ok(path.finish()?)
    }
    fn temporary_schema(&self) -> Option<String> {
        self.temporary_namespace_allocated()
            .then(|| self.temporary_schema_name())
    }
    fn temporary_schema_with_control(
        &self,
        control: &uqa_core::memory::ProductionControl<'_>,
    ) -> Result<Option<uqa_core::memory::Produced<String>>, SQLError> {
        control.check()?;
        self.temporary_namespace_allocated()
            .then(|| {
                control
                    .format(format_args!("pg_temp_{}", self.session_id))
                    .map_err(Into::into)
            })
            .transpose()
    }
    fn schema_exists_with_control(
        &self,
        schema: &str,
        control: &uqa_core::memory::ProductionControl<'_>,
    ) -> Result<bool, SQLError> {
        let (catalog, temporary) = self.routine_namespace_state(control)?;
        Ok(uqa_execution::catalog::namespaces::namespace_schema_exists(
            &catalog,
            temporary.as_ref().map_or("", |name| name.as_str()),
            schema,
        ))
    }
    fn require_schema_usage(&self, schema: &str, role: &RoleReference) -> Result<(), SQLError> {
        self.require_schema_privilege(schema, role, SchemaAclPrivilege::Usage)
    }
    fn schema_has_usage(&self, schema: &str, role: &RoleReference) -> bool {
        self.schema_has_privilege_for_role(schema, role, SchemaAclPrivilege::Usage)
    }
    fn schema_has_usage_with_control(
        &self,
        schema: &str,
        role: &RoleReference,
        control: &uqa_core::memory::ProductionControl<'_>,
    ) -> Result<bool, SQLError> {
        let (catalog, temporary) = self.routine_namespace_state(control)?;
        uqa_execution::catalog::namespaces::schema_has_privilege_with_control(
            &catalog,
            temporary.as_ref().map_or("", |name| name.as_str()),
            schema,
            role,
            SchemaAclPrivilege::Usage,
            control,
        )
    }
    fn require_schema_usage_with_control(
        &self,
        schema: &str,
        role: &RoleReference,
        control: &uqa_core::memory::ProductionControl<'_>,
    ) -> Result<(), SQLError> {
        uqa_sql::routines::lifecycle::names::require_routine_schema_usage_with_control(
            self, schema, role, control,
        )
    }

    fn routine_type_display(&self, type_name: &str) -> String {
        // Pseudo-types such as `anyelement` have no catalog column type and keep their names.
        uqa_execution::catalog::projection::resolve_catalog_column_type(
            &self.catalog_execution(),
            type_name,
        )
        .map_or_else(|| type_name.to_string(), |ty| ty.display_name())
    }
    fn routine_identity_display(&self, oid: u32) -> Result<String, SQLError> {
        uqa_execution::catalog::projection::resolve_regtype_output(
            &self.catalog_execution(),
            &uqa_sql::ast::ColumnType::Regprocedure,
            i64::from(oid),
        )
        .map_err(SQLError::Internal)?
        .ok_or_else(|| SQLError::Internal(format!("routine OID {oid} has no catalog identity")))
    }
}

use uqa_execution::routines::removal::context::{
    RoutineDropNotices, RoutineRegistryPublication, RoutineRegistryState, RoutineRegistryWrite,
    RoutineRemovalContext,
};
use uqa_sql::routines::lifecycle::RoutineRegistry;

impl Engine {
    fn routine_namespace_state(
        &self,
        control: &uqa_core::memory::ProductionControl<'_>,
    ) -> Result<
        (
            uqa_execution::catalog::CatalogReadView,
            Option<uqa_core::memory::Produced<String>>,
        ),
        SQLError,
    > {
        control.check()?;
        Ok((
            self.catalog_execution().catalog.catalog_snapshot(),
            <Self as RoutineNameCatalog>::temporary_schema_with_control(self, control)?,
        ))
    }
    pub(crate) fn routine_removal_context(&self) -> RoutineRemovalContext<'_> {
        RoutineRemovalContext {
            deletion: self,
            names: self,
            registry: self,
            publication: self,
            roles: self,
            catalog: self.catalog_execution(),
            bodies: self.routine_rewrite_context(),
            notices: self,
            changes: self,
        }
    }
}
impl RoutineRegistryState for Engine {
    fn routine_snapshot(&self) -> RoutineRegistry {
        self.durable.sql_user_functions.read().clone()
    }
    fn routines_write(&self) -> RoutineRegistryWrite<'_> {
        Box::new(self.durable.sql_user_functions.write())
    }
}
impl RoutineRegistryPublication for Engine {
    fn persist_routine_definitions(&self, registry: &RoutineRegistry) -> Result<(), SQLError> {
        self.persist_sql_functions_snapshot(registry)
    }
}
impl RoutineDropNotices for Engine {
    fn routine_drop_notice(&self, notice: uqa_sql::SQLNotice) {
        self.push_sql_notice(notice);
    }
}
