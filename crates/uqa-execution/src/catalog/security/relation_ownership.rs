//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Read actual relation owners before applying a command's requested relation kind.

use super::table_inquiry::TablePrivilegeContext;
use uqa_core::RelationIdentity;
use uqa_sql::{
    catalog::{
        roles::{role_inherits, RoleIdentity, RoleReference},
        security::ownership::require_relation_ownership,
        SystemRelation,
    },
    SQLError,
};

impl TablePrivilegeContext<'_> {
    pub fn ensure_relation_owner(
        &self,
        relation: &RelationIdentity,
        kind: &str,
    ) -> Result<(), SQLError> {
        let owner = self.relation_owner(relation, kind)?;
        let roles = self.roles.role_definitions();
        let owner = RoleReference::from_identity(owner, &roles)?;
        let memberships = self.roles.role_memberships();
        require_relation_ownership(
            &relation.name,
            kind,
            role_inherits(&roles, &memberships, &self.names.current_role(), &owner),
        )
    }

    fn relation_owner(
        &self,
        relation: &RelationIdentity,
        kind: &str,
    ) -> Result<RoleIdentity, SQLError> {
        if let Some(system) = SystemRelation::at(&relation.schema, &relation.name) {
            return Ok(self.registry.system_relation_security(system).role_owner);
        }
        let owner = match kind {
            "table" => self
                .registry
                .tables()
                .get(relation)
                .map(super::table_inquiry::TablePrivilegeState::role_owner),
            "view" | "materialized view" => self
                .registry
                .views()
                .get(relation)
                .map(|view| view.security.role_owner),
            "foreign table" => self
                .registry
                .foreign_security()
                .get(relation)
                .map(|security| security.role_owner),
            "sequence" => self
                .sequences
                .security
                .security_read()
                .get(relation)
                .map(|security| security.role_owner),
            "index" => {
                let catalog = self.catalog.catalog_read_view();
                let index = catalog
                    .snapshot()
                    .definitions
                    .catalog_indexes
                    .get(relation)
                    .cloned()
                    .map_or_else(
                        || catalog.constraint_index(relation),
                        |index| Ok(Some(index)),
                    )?
                    .ok_or_else(|| missing_owner(relation))?;
                let table = RelationIdentity::from_legacy_name(&index.table_name)
                    .map_err(SQLError::Internal)?;
                let (_, parent_kind) = self
                    .catalog
                    .resolve_bound_relation_kind(&index.table_name)?
                    .into_found()
                    .ok_or_else(|| missing_owner(&table))?;
                if !matches!(parent_kind, "table" | "materialized view") {
                    return Err(missing_owner(&table));
                }
                return self.relation_owner(&table, parent_kind);
            }
            _ => None,
        };
        owner.ok_or_else(|| missing_owner(relation))
    }
}

fn missing_owner(relation: &RelationIdentity) -> SQLError {
    SQLError::Internal(format!(
        "relation `{}` disappeared during ownership validation",
        relation.qualified_name()
    ))
}
