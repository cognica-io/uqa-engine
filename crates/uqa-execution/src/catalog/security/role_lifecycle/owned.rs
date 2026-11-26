//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! DROP OWNED resolves the complete role list before observing shared dependencies or changing any catalog tuples.

use super::context::RoleExecutionContext;
use crate::{
    catalog::{
        projection::{CatalogDependencies, CatalogObject, RelationKind},
        security::{
            database_lifecycle, roles::locking::RoleLockContext, table_grants::TableGrantContext,
        },
    },
    routines::privileges::RoutinePrivilegeContext,
    row_locks::RelationLockMode,
    schema::{
        deletion::{catalog_dependencies, perform_deletion, CatalogRemovalContext},
        namespaces::privileges,
        types,
    },
};
use std::collections::{BTreeMap, BTreeSet};
use uqa_core::catalog_role::RoleIdentity;
use uqa_sql::{
    ast::DropOwnedStmt,
    catalog::{
        dependencies::{
            ObjectAddress, SharedDependencyKind, DATABASE_CLASS, PROCEDURE_CLASS, RELATION_CLASS,
            ROLE_CLASS, ROLE_MEMBERSHIP_CLASS,
        },
        roles::{resolve_role_specification, role_inherits},
        DATABASE_OID,
    },
    SQLError,
};

pub struct DropOwnedContext<'a> {
    pub roles: RoleExecutionContext<'a>,
    pub removal: CatalogRemovalContext<'a>,
    pub schemas: privileges::SchemaPrivilegeContext<'a>,
    pub database: database_lifecycle::DatabasePrivilegeContext<'a>,
    pub tables: TableGrantContext<'a>,
    pub types: types::TypeLifecycleContext<'a>,
    pub routines: RoutinePrivilegeContext<'a>,
}

struct Targets {
    names: BTreeSet<String>,
    identities: BTreeSet<RoleIdentity>,
    oids: BTreeSet<u32>,
}

impl Targets {
    fn bind(
        context: &RoleExecutionContext<'_>,
        statement: &DropOwnedStmt,
    ) -> Result<Self, SQLError> {
        let locks = RoleLockContext {
            roles: context.analysis.roles,
            session: context.locks,
        };
        let current = context.analysis.names.current_role();
        let definitions = context.analysis.roles.role_definitions();
        let selected = statement
            .roles
            .iter()
            .map(|spec| resolve_role_specification(context.analysis.names, spec).bind(&definitions))
            .collect::<Result<Vec<_>, _>>()?;
        drop(definitions);
        let ordered: BTreeMap<_, _> = selected.iter().map(|role| (role.oid, role)).collect();
        for role in ordered.values() {
            locks.lock(role, RelationLockMode::AccessExclusive)?;
        }
        let definitions = context.analysis.roles.role_definitions();
        let memberships = context.analysis.roles.role_memberships();
        for role in &selected {
            role.revalidate(&definitions)?;
            if !role_inherits(&definitions, &memberships, &current, role) {
                return Err(SQLError::Diagnostic {
                    sqlstate: "42501".into(),
                    message: "permission denied to drop objects".into(),
                    detail: Some(format!(
                        "Only roles with privileges of role \"{}\" may drop objects owned by it.",
                        role.name
                    )),
                    hint: None,
                });
            }
        }
        for role in &selected {
            if CatalogDependencies::role_is_pinned(role.oid) {
                return Err(SQLError::Routine { sqlstate: "2BP01".into(), message: format!("cannot drop objects owned by role {} because they are required by the database system", uqa_sql::expr::quote_ident(&role.name)) });
            }
        }
        Ok(Self {
            names: selected.iter().map(|role| role.name.clone()).collect(),
            identities: selected.iter().map(|role| role.identity()).collect(),
            oids: ordered.into_keys().collect(),
        })
    }

    fn objects(
        &self,
        catalog: &CatalogDependencies,
        kind: SharedDependencyKind,
    ) -> BTreeSet<ObjectAddress> {
        catalog
            .shared()
            .iter()
            .filter(|dependency| {
                dependency.kind == kind
                    && dependency.referenced.class_id == ROLE_CLASS
                    && self.oids.contains(&dependency.referenced.object_id)
                    && (i64::from(dependency.database) == DATABASE_OID
                        || (kind == SharedDependencyKind::Acl && dependency.database == 0))
            })
            .map(|dependency| dependency.dependent)
            .collect()
    }
}

impl DropOwnedContext<'_> {
    pub fn execute(&self, statement: &DropOwnedStmt) -> Result<(), SQLError> {
        let targets = Targets::bind(&self.roles, statement)?;
        self.roles.publication.prepare_writer()?;
        let catalog = catalog_dependencies(&self.removal.catalog)?;
        let acls: BTreeSet<_> = targets
            .objects(&catalog, SharedDependencyKind::Acl)
            .into_iter()
            .map(|address| ObjectAddress::whole(address.class_id, address.object_id))
            .collect();
        for address in acls {
            self.remove_acl(&catalog, &targets, address)?;
        }
        perform_deletion(
            &self.removal,
            |catalog| {
                Ok(targets
                    .objects(catalog, SharedDependencyKind::Owner)
                    .into_iter()
                    .collect())
            },
            statement.cascade,
        )
    }

    fn remove_acl(
        &self,
        catalog: &CatalogDependencies,
        targets: &Targets,
        address: ObjectAddress,
    ) -> Result<(), SQLError> {
        match address.class_id {
            DATABASE_CLASS => {
                return database_lifecycle::remove_role_grants(&self.database, &targets.names)
            }
            ROLE_MEMBERSHIP_CLASS => {
                return self.remove_membership(address.object_id, &targets.identities)
            }
            PROCEDURE_CLASS => {
                return crate::routines::privileges::remove_role_grants(
                    &self.routines,
                    address.object_id,
                    &targets.identities,
                )
            }
            _ => {}
        }
        match catalog.catalog_object(address) {
            Some(CatalogObject::Relation {
                identity,
                kind: RelationKind::Sequence,
                ..
            }) => self
                .tables
                .sequences
                .remove_role_grants(&identity, &targets.names),
            Some(CatalogObject::Relation { identity, kind, .. }) => self.tables.remove_role_grants(
                identity,
                kind.description(),
                address.object_id,
                &targets.names,
            ),
            Some(CatalogObject::Type(oid)) => {
                types::remove_role_grants(&self.types, oid, &targets.identities)
            }
            Some(CatalogObject::Schema(name)) => {
                privileges::remove_role_grants(&self.schemas, &name, &targets.names)
            }
            None if address.class_id == RELATION_CLASS => {
                if let Some(system) = uqa_sql::catalog::SystemRelation::all()
                    .find(|relation| relation.oid() == i64::from(address.object_id))
                {
                    return self.tables.remove_role_grants(
                        uqa_core::RelationIdentity::new(system.namespace(), system.name()),
                        system.kind(),
                        address.object_id,
                        &targets.names,
                    );
                }
                Ok(())
            }
            other => Err(SQLError::Internal(format!(
                "unhandled role ACL dependency {address:?} ({other:?})"
            ))),
        }
    }

    fn remove_membership(&self, oid: u32, roles: &BTreeSet<RoleIdentity>) -> Result<(), SQLError> {
        let mut memberships = self.roles.registry.write_memberships();
        let Some((key, _)) = memberships.iter().find(|(_, edge)| {
            edge.oid == i64::from(oid) && roles.contains(&edge.grantor.identity())
        }) else {
            return Ok(());
        };
        let key = *key;
        let mut next = memberships.clone();
        // A membership depends on its own grantor. PostgreSQL deletes that pg_auth_members row directly, leaving rows issued by other grantors in place.
        next.remove(&key)?;
        self.roles
            .publication
            .persist_memberships(&memberships, &next)?;
        **memberships = next;
        drop(memberships);
        self.roles.publication.catalog_changed();
        Ok(())
    }
}
