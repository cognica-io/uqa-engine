//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Captured routine catalog and OID adapters for SQL-owned privilege inquiry.

use crate::catalog::{
    context::CatalogContext,
    projection::{resolve_regprocedure_input_oid, routine_oid_exists, user_routine_catalog_oid},
    CatalogReadView,
};
use uqa_core::Value;
use uqa_sql::{
    routines::privilege_inquiry::{
        RoutinePrivilegeCatalog, RoutinePrivilegeInquiry, RoutinePrivileges,
    },
    SQLError,
};

struct RoutineCatalog<'a, 'b> {
    context: &'a CatalogContext<'b>,
    catalog: CatalogReadView,
}

impl RoutinePrivilegeCatalog for RoutineCatalog<'_, '_> {
    fn resolve_routine_name(&self, name: &str) -> Result<i64, SQLError> {
        resolve_regprocedure_input_oid(self.context, name)
    }

    fn routine_privileges(&self, oid: i64) -> Result<Option<RoutinePrivileges<'_>>, SQLError> {
        let definitions = &self.catalog.snapshot().definitions;
        for routine in definitions.sql_user_functions.values().flatten() {
            if user_routine_catalog_oid(routine)? == oid {
                return Ok(Some(RoutinePrivileges {
                    owner: uqa_sql::routines::security::bound_routine_owner(&routine.def)?,
                    execute_acl: routine.def.execute_acl.as_deref(),
                }));
            }
        }
        if !routine_oid_exists(self.context, oid)? {
            return Ok(None);
        }
        let owner = definitions
            .roles
            .values()
            .find(|role| role.oid == 10)
            .ok_or_else(|| SQLError::Internal("routine catalog has no bootstrap owner".into()))?;
        Ok(Some(RoutinePrivileges {
            owner: owner.identity(),
            execute_acl: None,
        }))
    }
}

pub fn has_function_privilege_value(
    context: &CatalogContext<'_>,
    arguments: &[Value],
) -> Result<Value, SQLError> {
    let catalog = RoutineCatalog {
        context,
        catalog: context.catalog_read_view(),
    };
    let definitions = &catalog.catalog.snapshot().definitions;
    RoutinePrivilegeInquiry {
        current_user: &context.current_role(),
        roles: &definitions.roles,
        memberships: &definitions.role_memberships,
        catalog: &catalog,
    }
    .has_function_privilege_value(arguments)
}
