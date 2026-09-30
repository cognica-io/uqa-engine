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
        self.session.state.read().search_path.clone()
    }
    fn require_schema_usage(&self, schema: &str, role: &RoleReference) -> Result<(), SQLError> {
        self.require_schema_privilege(schema, role, SchemaAclPrivilege::Usage)
    }
    fn schema_has_usage(&self, schema: &str, role: &RoleReference) -> bool {
        self.schema_has_privilege_for_role(schema, role, SchemaAclPrivilege::Usage)
    }
    fn routine_type_display(&self, type_name: &str) -> String {
        // Pseudo-types such as `anyelement` have no catalog column type and keep their names.
        uqa_execution::catalog::projection::resolve_catalog_column_type(
            &self.catalog_execution(),
            type_name,
        )
        .map_or_else(|| type_name.to_string(), |ty| ty.display_name())
    }
}

use uqa_execution::routines::removal::context::{
    RoutineDropNotices, RoutineRegistryPublication, RoutineRegistryState, RoutineRegistryWrite,
    RoutineRemovalContext,
};
use uqa_sql::routines::lifecycle::RoutineRegistry;

impl Engine {
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
