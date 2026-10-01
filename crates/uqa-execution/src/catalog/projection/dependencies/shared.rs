//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `pg_shdepend` rows as `recordDependencyOnOwner` and `updateAclDependencies` record them: each owner, and each role an object's privileges name as a grantee or grantor other than its owner, unless the role is pinned. Role memberships depend on their grantors.

use super::{DependencyBuilder, MemberObject};
use std::collections::BTreeSet;
use uqa_core::catalog_role::RoleIdentity;
use uqa_sql::catalog::dependencies::{
    ObjectAddress, SharedDependency, SharedDependencyKind, DATABASE_CLASS, NAMESPACE_CLASS,
    PROCEDURE_CLASS, RELATION_CLASS, ROLE_CLASS, ROLE_MEMBERSHIP_CLASS, TYPE_CLASS,
};
use uqa_sql::SQLError;

impl DependencyBuilder<'_> {
    pub(super) fn record_shared(&mut self) -> Result<(), SQLError> {
        let database = super::catalog_oid(uqa_sql::catalog::DATABASE_OID)?;
        self.record_shared_namespaces(database);
        self.record_shared_relations(database);
        self.record_shared_objects(database)?;
        self.record_shared_memberships()
    }

    /// The database, which every database shares, and its schemas.
    fn record_shared_namespaces(&mut self, database: u32) {
        let snapshot = self.catalog.snapshot();
        let definitions = &snapshot.definitions;
        let security = definitions.database_security.as_ref();
        self.record_owned(
            (0, ObjectAddress::whole(DATABASE_CLASS, database)),
            security.role_owner,
            security
                .acl
                .iter()
                .flatten()
                .map(|entry| (entry.role, entry.grantor)),
        );
        for (name, security) in definitions.schemas.iter() {
            let Some(oid) = self.objects.namespace_oid(name) else {
                continue;
            };
            self.record_owned(
                (database, ObjectAddress::whole(NAMESPACE_CLASS, oid)),
                security.role_owner,
                security
                    .acl
                    .iter()
                    .flatten()
                    .map(|entry| (entry.role, entry.grantor)),
            );
        }
    }

    /// Tables, views, materialized views and foreign tables with their columns' privileges, and privileges granted on the system catalogs, which the bootstrap superuser owns.
    fn record_shared_relations(&mut self, database: u32) {
        let snapshot = self.catalog.snapshot();
        let definitions = &snapshot.definitions;
        let relations = snapshot
            .tables
            .iter()
            .map(|(identity, table)| (identity, table.security.as_ref().clone()))
            .chain(
                definitions
                    .views
                    .iter()
                    .map(|(identity, view)| (identity, view.security.clone())),
            )
            .chain(
                definitions
                    .foreign_table_security
                    .iter()
                    .map(|(identity, security)| (identity, security.clone())),
            )
            .collect::<Vec<_>>();
        for (identity, security) in relations {
            let Some(oid) = self.objects.relation_oid(identity) else {
                continue;
            };
            self.record_role(
                database,
                ObjectAddress::whole(RELATION_CLASS, oid),
                security.role_owner.oid,
                SharedDependencyKind::Owner,
            );
            let columns = self
                .objects
                .relation(oid)
                .map(|relation| {
                    relation
                        .columns
                        .iter()
                        .map(|column| column.name.clone())
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            self.record_relation_privileges(database, oid, &security, &columns);
        }
        for (identity, entry) in definitions.system_relation_security.iter() {
            let Some(relation) =
                uqa_sql::catalog::SystemRelation::at(&identity.schema, &identity.name)
            else {
                continue;
            };
            let Ok(oid) = u32::try_from(relation.oid()) else {
                continue;
            };
            let security = entry.security(relation);
            self.record_relation_privileges(database, oid, &security, &relation.column_names());
        }
    }

    /// The privileges of a relation and of each of its columns, named in `columns` in column-number order.
    fn record_relation_privileges(
        &mut self,
        database: u32,
        oid: u32,
        security: &uqa_sql::catalog::security::BoundTableSecurity,
        columns: &[String],
    ) {
        self.record_privileges(
            (database, ObjectAddress::whole(RELATION_CLASS, oid)),
            Some(security.role_owner),
            security
                .acl
                .iter()
                .flatten()
                .map(|entry| (entry.role, entry.grantor)),
        );
        for (column, acl) in &security.column_acls {
            let Some(number) = columns
                .iter()
                .position(|name| name == column)
                .and_then(|index| i32::try_from(index + 1).ok())
            else {
                continue;
            };
            self.record_privileges(
                (database, ObjectAddress::column(oid, number)),
                Some(security.role_owner),
                acl.iter().map(|entry| (entry.role, entry.grantor)),
            );
        }
    }

    /// Sequences, enums, domains, composite types and routines. A composite type's relation records no owner of its own: `heap_create_with_catalog` leaves ownership to the row type.
    fn record_shared_objects(&mut self, database: u32) -> Result<(), SQLError> {
        let catalog = self.catalog;
        let snapshot = catalog.snapshot();
        for (identity, security) in snapshot.definitions.sequence_security.iter() {
            let Some(oid) = self.objects.relation_oid(identity) else {
                continue;
            };
            self.record_owned(
                (database, ObjectAddress::whole(RELATION_CLASS, oid)),
                security.role_owner,
                security
                    .acl
                    .iter()
                    .flatten()
                    .map(|entry| (entry.role, entry.grantor)),
            );
        }
        for definition in catalog.enums() {
            self.record_owned(
                (database, ObjectAddress::whole(TYPE_CLASS, definition.oid)),
                definition.owner,
                definition
                    .usage_acl
                    .iter()
                    .flatten()
                    .map(|entry| (entry.role, entry.grantor)),
            );
        }
        for domain in catalog.domains() {
            self.record_owned(
                (database, ObjectAddress::whole(TYPE_CLASS, domain.oid)),
                domain.owner,
                domain
                    .usage_acl
                    .iter()
                    .flatten()
                    .map(|entry| (entry.role, entry.grantor)),
            );
        }
        for composite in catalog.composites() {
            self.record_owned(
                (database, ObjectAddress::whole(TYPE_CLASS, composite.oid)),
                composite.owner,
                composite
                    .usage_acl
                    .iter()
                    .flatten()
                    .map(|entry| (entry.role, entry.grantor)),
            );
        }
        for function in catalog.all_sql_functions() {
            let oid = super::catalog_oid(super::super::user_routine_catalog_oid(&function)?)?;
            let owner = uqa_sql::routines::security::bound_routine_owner(&function.def)?;
            self.record_owned(
                (database, ObjectAddress::whole(PROCEDURE_CLASS, oid)),
                owner,
                function
                    .def
                    .execute_acl
                    .iter()
                    .flatten()
                    .map(|entry| (entry.role, entry.grantor)),
            );
        }
        Ok(())
    }

    /// A role membership, which every database shares, has no owner; its grantor is recorded as privileges are.
    fn record_shared_memberships(&mut self) -> Result<(), SQLError> {
        let snapshot = self.catalog.snapshot();
        for membership in snapshot.definitions.role_memberships.values() {
            let oid = super::catalog_oid(membership.oid)?;
            let (member, role) = (
                super::catalog_oid(membership.member.identity().oid)?,
                super::catalog_oid(membership.role.identity().oid)?,
            );
            self.objects.add_member(
                ROLE_MEMBERSHIP_CLASS,
                oid,
                MemberObject::Membership { member, role },
            );
            self.record_privileges(
                (0, ObjectAddress::whole(ROLE_MEMBERSHIP_CLASS, oid)),
                None,
                [(None, membership.grantor.identity())],
            );
        }
        Ok(())
    }

    /// `recordDependencyOnOwner`, then `recordDependencyOnNewAcl`.
    fn record_owned(
        &mut self,
        (database, object): (u32, ObjectAddress),
        owner: RoleIdentity,
        acl: impl IntoIterator<Item = (Option<RoleIdentity>, RoleIdentity)>,
    ) {
        self.record_role(database, object, owner.oid, SharedDependencyKind::Owner);
        self.record_privileges((database, object), Some(owner), acl);
    }

    /// `updateAclDependencies`: each distinct role an ACL names as a grantee or grantor, except PUBLIC and the owner, whose ownership is recorded instead.
    fn record_privileges(
        &mut self,
        (database, object): (u32, ObjectAddress),
        owner: Option<RoleIdentity>,
        acl: impl IntoIterator<Item = (Option<RoleIdentity>, RoleIdentity)>,
    ) {
        let mut roles = BTreeSet::new();
        for (grantee, grantor) in acl {
            roles.extend(grantee.map(|grantee| grantee.oid));
            roles.insert(grantor.oid);
        }
        if let Some(owner) = owner {
            roles.remove(&owner.oid);
        }
        for role in roles {
            self.record_role(database, object, role, SharedDependencyKind::Acl);
        }
    }

    /// A dependency on a pinned role is not recorded.
    fn record_role(
        &mut self,
        database: u32,
        dependent: ObjectAddress,
        role: i64,
        kind: SharedDependencyKind,
    ) {
        let Ok(role) = u32::try_from(role) else {
            return;
        };
        let referenced = ObjectAddress::whole(ROLE_CLASS, role);
        if self.objects.is_unpinned(referenced) {
            self.shared.push(SharedDependency {
                database,
                dependent,
                referenced,
                kind,
            });
        }
    }
}
