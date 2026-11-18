//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Borrowed authority validation uses role incarnations and admitted grant graphs.

use super::{
    database::{BoundDatabaseSecurity, DatabasePrivileges},
    BoundSchemaSecurity, BoundSequenceSecurity, BoundTableSecurity,
};
use crate::{
    ast::ObjectAclEntry,
    catalog::{
        roles::{RoleDefinition, RoleDefinitions, RoleIdentity, RoleMemberships},
        work::{inline, Work},
    },
    SQLError,
};
use uqa_core::{
    catalog_acl::TablePrivileges,
    catalog_role::BoundAclEntry,
    catalog_schema::SchemaPrivileges,
    catalog_sequence::SequencePrivileges,
    memory::{BudgetedMap, MemoryBudget, ProductionControl},
    CancellationToken,
};

mod columns;
mod grants;
mod memberships;
#[cfg(test)]
mod tests;

/// A validation-only index borrows the selected role definitions. It never resolves an old incarnation through a recycled OID or display name, copies catalog payloads, or executes a callback.
pub struct AuthorityCatalog<'a> {
    roles: BudgetedMap<i64, &'a RoleDefinition>,
    memory: &'a MemoryBudget,
    cancellation: &'a CancellationToken,
}

impl<'a> AuthorityCatalog<'a> {
    pub fn new(
        roles: &'a RoleDefinitions,
        memory: &'a MemoryBudget,
        cancellation: &'a CancellationToken,
    ) -> Result<Self, SQLError> {
        inline(Self::new_with_work(roles, memory, cancellation, &mut ()))
    }

    /// Index the selected role incarnations without borrowing a later catalog generation.
    pub async fn new_with_work<W: Work + Send>(
        roles: &'a RoleDefinitions,
        memory: &'a MemoryBudget,
        cancellation: &'a CancellationToken,
        work: &mut W,
    ) -> Result<Self, SQLError> {
        cancellation.check()?;
        let mut result = Self {
            roles: BudgetedMap::new(memory),
            memory,
            cancellation,
        };
        let mut objects = BudgetedMap::new(memory);
        let control = ProductionControl::new(memory, cancellation, cancellation);
        for (name, role) in roles {
            result.checkpoint(work).await?;
            if name.len() != role.name.len()
                || !crate::catalog::work::compare_text(work, name, &role.name, &control)
                    .await?
                    .is_eq()
                || !valid_identity(role.identity())
                || role.revision == 0
            {
                return Err(invalid("invalid role definition"));
            }
            if result.roles.insert(role.oid, role)?.is_some()
                || objects.insert(role.object_id, ())?.is_some()
            {
                return Err(invalid("duplicate role identity"));
            }
        }
        crate::catalog::work::retire_entries(objects.into_entries(), &control, work).await?;
        cancellation.check()?;
        Ok(result)
    }

    fn control(&self) -> ProductionControl<'a> {
        ProductionControl::new(self.memory, self.cancellation, self.cancellation)
    }

    /// Retire the selected role index after successful validation using the original operation's work channel.
    pub async fn retire(self, work: &mut impl Work) -> Result<(), SQLError> {
        let control = self.control();
        crate::catalog::work::retire_entries(self.roles.into_entries(), &control, work).await
    }

    /// Bound the next authority row or edge with the selected catalog's original cancellation owner.
    pub async fn checkpoint(&self, work: &mut impl Work) -> Result<(), SQLError> {
        self.cancellation.check()?;
        work.checkpoint().await?;
        self.cancellation.check()?;
        Ok(())
    }

    pub fn role(&self, identity: RoleIdentity) -> Result<(), SQLError> {
        self.cancellation.check()?;
        if self
            .roles
            .get(&identity.oid)
            .is_none_or(|role| role.identity() != identity)
        {
            return Err(invalid("missing or replaced role incarnation"));
        }
        Ok(())
    }

    /// Memberships preserve deleted target/member incarnations without rebinding them to new roles; grantors must still exist. The retained role graph must be acyclic.
    pub fn memberships(&self, memberships: &RoleMemberships) -> Result<(), SQLError> {
        inline(self.memberships_with_work(memberships, &mut ()))
    }

    pub async fn memberships_with_work(
        &self,
        memberships: &RoleMemberships,
        work: &mut impl Work,
    ) -> Result<(), SQLError> {
        memberships::validate(self, memberships, work).await
    }

    pub fn object(
        &self,
        owner: RoleIdentity,
        acl: Option<&[ObjectAclEntry]>,
    ) -> Result<(), SQLError> {
        inline(self.object_with_work(owner, acl, &mut ()))
    }

    pub async fn object_with_work(
        &self,
        owner: RoleIdentity,
        acl: Option<&[ObjectAclEntry]>,
        work: &mut impl Work,
    ) -> Result<(), SQLError> {
        self.grants(
            owner,
            acl.into_iter().flatten().map(|entry| grants::Grant {
                grantee: entry.role,
                grantor: entry.grantor,
                privileges: 1,
                options: u16::from(entry.grant_option),
            }),
            std::iter::empty(),
            work,
        )
        .await
    }

    pub fn schema(&self, value: &BoundSchemaSecurity) -> Result<(), SQLError> {
        inline(self.schema_with_work(value, &mut ()))
    }

    pub async fn schema_with_work(
        &self,
        value: &BoundSchemaSecurity,
        work: &mut impl Work,
    ) -> Result<(), SQLError> {
        if value.tuple.is_some_and(|tuple| !tuple.is_valid()) {
            return Err(invalid("invalid schema tuple identity"));
        }
        self.grants(
            value.role_owner,
            acl(value.acl.as_deref(), schema_bits),
            std::iter::empty(),
            work,
        )
        .await
    }

    pub fn sequence(&self, value: &BoundSequenceSecurity) -> Result<(), SQLError> {
        inline(self.sequence_with_work(value, &mut ()))
    }

    pub async fn sequence_with_work(
        &self,
        value: &BoundSequenceSecurity,
        work: &mut impl Work,
    ) -> Result<(), SQLError> {
        self.grants(
            value.role_owner,
            acl(value.acl.as_deref(), sequence_bits),
            std::iter::empty(),
            work,
        )
        .await
    }

    pub fn database(&self, value: &BoundDatabaseSecurity) -> Result<(), SQLError> {
        inline(self.database_with_work(value, &mut ()))
    }

    pub async fn database_with_work(
        &self,
        value: &BoundDatabaseSecurity,
        work: &mut impl Work,
    ) -> Result<(), SQLError> {
        self.grants(
            value.role_owner,
            acl(value.acl.as_deref(), database_bits),
            std::iter::empty(),
            work,
        )
        .await
    }

    pub fn system_relation(
        &self,
        relation: crate::catalog::SystemRelation,
        value: &super::system_relations::SystemRelationSecurity,
    ) -> Result<(), SQLError> {
        inline(self.system_relation_with_work(relation, value, &mut ()))
    }

    pub async fn system_relation_with_work(
        &self,
        relation: crate::catalog::SystemRelation,
        value: &super::system_relations::SystemRelationSecurity,
        work: &mut impl Work,
    ) -> Result<(), SQLError> {
        let owner = RoleIdentity::BOOTSTRAP;
        let (default, count) = relation.bootstrap_acl();
        let table = value
            .table
            .as_ref()
            .map_or(&default[..count], |entry| entry.acl.as_slice());
        if value
            .table
            .as_ref()
            .is_some_and(|entry| entry.revision == [0; 16])
        {
            return Err(invalid("invalid system relation ACL revision"));
        }
        self.grants(
            owner,
            acl(Some(table), table_bits),
            std::iter::empty(),
            work,
        )
        .await?;
        let control = uqa_core::memory::ProductionControl::new(
            self.memory,
            self.cancellation,
            self.cancellation,
        );
        let columns = relation.column_names_with_control(&control)?;
        for (column, entry) in &value.columns {
            self.checkpoint(work).await?;
            if entry.revision == [0; 16] || !columns.contains(column) {
                return Err(invalid("invalid system column ACL identity"));
            }
            for grant in &entry.acl {
                self.checkpoint(work).await?;
                if (table_bits(grant.privileges) | table_bits(grant.grant_options)) & !0x27 != 0 {
                    return Err(invalid("column ACL contains a relation-only privilege"));
                }
            }
            self.grants(
                owner,
                acl(Some(&entry.acl), table_bits),
                acl(Some(table), table_bits),
                work,
            )
            .await?;
        }
        Ok(())
    }
}

fn acl<P: Copy>(
    entries: Option<&[BoundAclEntry<P>]>,
    bits: fn(P) -> u16,
) -> impl Iterator<Item = grants::Grant> + Clone + '_ {
    entries
        .into_iter()
        .flatten()
        .map(move |entry| grants::Grant {
            grantee: entry.role,
            grantor: entry.grantor,
            privileges: bits(entry.privileges),
            options: bits(entry.grant_options),
        })
}

fn table_bits(value: TablePrivileges) -> u16 {
    u16::from(value.select)
        | u16::from(value.insert) << 1
        | u16::from(value.update) << 2
        | u16::from(value.delete) << 3
        | u16::from(value.truncate) << 4
        | u16::from(value.references) << 5
        | u16::from(value.trigger) << 6
        | u16::from(value.maintain) << 7
}

fn schema_bits(value: SchemaPrivileges) -> u16 {
    u16::from(value.usage) | u16::from(value.create) << 1
}

fn sequence_bits(value: SequencePrivileges) -> u16 {
    u16::from(value.usage) | u16::from(value.select) << 1 | u16::from(value.update) << 2
}

fn database_bits(value: DatabasePrivileges) -> u16 {
    u16::from(value.connect) | u16::from(value.create) << 1 | u16::from(value.temporary) << 2
}

fn invalid(reason: &'static str) -> SQLError {
    SQLError::Internal(format!("invalid catalog authority: {reason}"))
}

fn valid_identity(identity: RoleIdentity) -> bool {
    identity.is_valid()
        && ((identity.oid == RoleIdentity::BOOTSTRAP.oid)
            == (identity.object_id == RoleIdentity::BOOTSTRAP.object_id))
}
