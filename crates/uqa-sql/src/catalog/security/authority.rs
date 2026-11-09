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
    catalog::roles::{RoleDefinition, RoleDefinitions, RoleIdentity, RoleMemberships},
    SQLError,
};
use uqa_core::{
    catalog_acl::TablePrivileges,
    catalog_role::BoundAclEntry,
    catalog_schema::SchemaPrivileges,
    catalog_sequence::SequencePrivileges,
    memory::{BudgetedMap, MemoryBudget},
    CancellationToken,
};

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
        cancellation.check()?;
        let mut result = Self {
            roles: BudgetedMap::new(memory),
            memory,
            cancellation,
        };
        let mut objects = BudgetedMap::new(memory);
        for (name, role) in roles {
            cancellation.check()?;
            if name != &role.name || !valid_identity(role.identity()) || role.revision == 0 {
                return Err(invalid("invalid role definition"));
            }
            if result.roles.insert(role.oid, role)?.is_some()
                || objects.insert(role.object_id, ())?.is_some()
            {
                return Err(invalid("duplicate role identity"));
            }
        }
        cancellation.check()?;
        Ok(result)
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
        memberships::validate(self, memberships)
    }

    pub fn object(
        &self,
        owner: RoleIdentity,
        acl: Option<&[ObjectAclEntry]>,
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
        )
    }

    pub fn schema(&self, value: &BoundSchemaSecurity) -> Result<(), SQLError> {
        if value.tuple.is_some_and(|tuple| !tuple.is_valid()) {
            return Err(invalid("invalid schema tuple identity"));
        }
        self.grants(
            value.role_owner,
            acl(value.acl.as_deref(), schema_bits),
            std::iter::empty(),
        )
    }

    pub fn sequence(&self, value: &BoundSequenceSecurity) -> Result<(), SQLError> {
        self.grants(
            value.role_owner,
            acl(value.acl.as_deref(), sequence_bits),
            std::iter::empty(),
        )
    }

    pub fn database(&self, value: &BoundDatabaseSecurity) -> Result<(), SQLError> {
        self.grants(
            value.role_owner,
            acl(value.acl.as_deref(), database_bits),
            std::iter::empty(),
        )
    }

    /// Column existence comes from the accompanying immutable row descriptor. Relation grants contribute column grant options, while each ACL retains its own ordered paths and duplicate-path boundary.
    pub fn table(
        &self,
        value: &BoundTableSecurity,
        mut column_exists: impl FnMut(&str) -> Result<bool, SQLError>,
    ) -> Result<(), SQLError> {
        if value.acl_revisions.relation == Some([0; 16]) {
            return Err(invalid("invalid relation ACL tuple identity"));
        }
        self.grants(
            value.role_owner,
            acl(value.acl.as_deref(), table_bits),
            std::iter::empty(),
        )?;
        for (column, revision) in &value.acl_revisions.columns {
            self.cancellation.check()?;
            if *revision == [0; 16] || !column_exists(column)? {
                return Err(invalid("invalid column ACL tuple identity"));
            }
        }
        for (column, entries) in &value.column_acls {
            self.cancellation.check()?;
            if !column_exists(column)? {
                return Err(invalid("column ACL references a missing column"));
            }
            for entry in entries {
                self.cancellation.check()?;
                if (table_bits(entry.privileges) | table_bits(entry.grant_options)) & !0x27 != 0 {
                    return Err(invalid("column ACL contains a relation-only privilege"));
                }
            }
            self.grants(
                value.role_owner,
                acl(Some(entries), table_bits),
                acl(value.acl.as_deref(), table_bits),
            )?;
        }
        self.cancellation.check()?;
        Ok(())
    }

    pub fn system_relation(
        &self,
        relation: crate::catalog::SystemRelation,
        value: &super::system_relations::SystemRelationSecurity,
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
        self.grants(owner, acl(Some(table), table_bits), std::iter::empty())?;
        let control = uqa_core::memory::ProductionControl::new(
            self.memory,
            self.cancellation,
            self.cancellation,
        );
        let columns = relation.column_names_with_control(&control)?;
        for (column, entry) in &value.columns {
            self.cancellation.check()?;
            if entry.revision == [0; 16] || !columns.contains(column) {
                return Err(invalid("invalid system column ACL identity"));
            }
            for grant in &entry.acl {
                self.cancellation.check()?;
                if (table_bits(grant.privileges) | table_bits(grant.grant_options)) & !0x27 != 0 {
                    return Err(invalid("column ACL contains a relation-only privilege"));
                }
            }
            self.grants(
                owner,
                acl(Some(&entry.acl), table_bits),
                acl(Some(table), table_bits),
            )?;
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
