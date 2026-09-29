//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Captured type catalog adapters for SQL-owned `has_type_privilege`.

use crate::catalog::{
    context::CatalogContext,
    projection::{resolve_regtype_oid, row_type_relation, type_privilege_oid},
    CatalogReadView,
};
use uqa_core::{catalog_role::RoleIdentity, Value};
use uqa_sql::{
    catalog::security::type_inquiry::{TypePrivilegeCatalog, TypePrivilegeInquiry, TypePrivileges},
    SQLError,
};

struct TypeCatalog<'a, 'b> {
    context: &'a CatalogContext<'b>,
    catalog: CatalogReadView,
}

impl TypePrivilegeCatalog for TypeCatalog<'_, '_> {
    fn resolve_type_name(&self, name: &str) -> Result<u32, SQLError> {
        resolve_regtype_oid(self.context, name)?
            .and_then(|oid| u32::try_from(oid).ok())
            .ok_or_else(|| SQLError::Routine {
                sqlstate: "42704".into(),
                message: format!("type \"{name}\" does not exist"),
            })
    }

    fn type_privileges(&self, oid: u32) -> Option<TypePrivileges<'_>> {
        let governing = type_privilege_oid(self.context, i64::from(oid)).ok()??;
        let governing = u32::try_from(governing).ok()?;
        if let Some(definition) = self
            .catalog
            .enums()
            .find(|definition| definition.oid == governing)
        {
            return Some(TypePrivileges {
                owner: definition.owner,
                usage_acl: definition.usage_acl.as_deref(),
            });
        }
        if let Some(domain) = self
            .catalog
            .domains()
            .find(|domain| domain.oid == governing)
        {
            return Some(TypePrivileges {
                owner: domain.owner,
                usage_acl: domain.usage_acl.as_deref(),
            });
        }
        let owner = row_type_relation(self.context, governing)
            .ok()
            .flatten()
            .map_or(RoleIdentity::BOOTSTRAP, |relation| relation.owner);
        Some(TypePrivileges {
            owner,
            usage_acl: None,
        })
    }
}

pub fn has_type_privilege_value(
    context: &CatalogContext<'_>,
    arguments: &[Value],
) -> Result<Value, SQLError> {
    let catalog = TypeCatalog {
        context,
        catalog: context.catalog_read_view(),
    };
    let definitions = &catalog.catalog.snapshot().definitions;
    TypePrivilegeInquiry {
        current_user: &context.current_role(),
        roles: &definitions.roles,
        memberships: &definitions.role_memberships,
        catalog: &catalog,
    }
    .has_type_privilege_value(arguments)
}
