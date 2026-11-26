//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Default ACL tuples are keyed by creating role, namespace and object class. Every grant in one tuple originates with its creating role.
use crate::{
    ast::{AlterDefaultPrivilegesStmt, DefaultAclObject, ObjectAclEntry},
    catalog::named_definitions::{
        sealed, CapturedDefinitions, DefinitionKey, DefinitionRegistry, StoredDefinition,
    },
    plan::copy::SyntaxProduction,
    SQLError,
};
use serde::{Deserialize, Serialize};
use uqa_core::{
    catalog_acl::TablePrivileges,
    catalog_role::{BoundAclEntry, RoleIdentity},
    catalog_schema::SchemaPrivileges,
    catalog_sequence::SequencePrivileges,
    memory::{Produced, ProductionControl},
};

mod rights;
pub mod stored;
#[cfg(test)]
mod tests;
pub use rights::DefaultRights;

pub type DefaultAcls = DefinitionRegistry<DefaultAclKey, DefaultAclRow>;
pub type CapturedDefaultAcls = CapturedDefinitions<DefaultAclKey, DefaultAclRow>;
pub type Entries = Vec<BoundAclEntry<DefaultRights>>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DefaultAclKey {
    pub owner: RoleIdentity,
    /// Zero identifies a database-wide default.
    pub namespace: u32,
    pub object: DefaultAclObject,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DefaultAclRow {
    pub oid: u32,
    pub temporary: bool,
    pub acl: Entries,
}

impl sealed::Key for DefaultAclKey {}
impl DefinitionKey for DefaultAclKey {
    type View = Self;
    fn payload_bytes(&self) -> usize {
        0
    }
}

impl sealed::Definition for DefaultAclRow {}
impl StoredDefinition<DefaultAclKey> for DefaultAclRow {
    fn is_temporary(&self, _: &DefaultAclKey) -> bool {
        self.temporary
    }
    fn copy_definition(&self, control: &ProductionControl<'_>) -> Result<Produced<Self>, SQLError> {
        let output = SyntaxProduction::new(*control);
        let copied = Self {
            oid: self.oid,
            temporary: self.temporary,
            acl: super::capture::acl_entries(&self.acl, &output)?,
        };
        output.finish(copied)
    }
}

impl DefaultAclKey {
    pub fn baseline(self) -> Entries {
        if self.namespace != 0 {
            return Vec::new();
        }
        let mut entries = Vec::new();
        if matches!(
            self.object,
            DefaultAclObject::Type | DefaultAclObject::Routine
        ) {
            entries.push(self.entry(None, self.object.rights()));
        }
        entries.push(self.entry(Some(self.owner), self.object.rights()));
        entries
    }

    fn entry(
        self,
        role: Option<RoleIdentity>,
        privileges: DefaultRights,
    ) -> BoundAclEntry<DefaultRights> {
        BoundAclEntry {
            role,
            grantor: self.owner,
            privileges,
            grant_options: DefaultRights::default(),
        }
    }

    pub fn change(
        self,
        old: Option<&DefaultAclRow>,
        statement: &AlterDefaultPrivilegesStmt,
        rights: DefaultRights,
        grantees: &[Option<RoleIdentity>],
    ) -> Result<Option<Entries>, SQLError> {
        self.object.check_namespace(self.namespace)?;
        let baseline = self.baseline();
        let mut next = old.map_or_else(|| baseline.clone(), |row| row.acl.clone());
        for &role in grantees {
            if role.is_none() && statement.is_grant && statement.grant_option {
                return Err(invalid_grant("grant options can only be granted to roles"));
            }
            let pos = next.binary_search_by_key(&role, |entry| entry.role);
            if statement.is_grant {
                let index = pos.unwrap_or_else(|index| {
                    next.insert(index, self.entry(role, DefaultRights::default()));
                    index
                });
                next[index].privileges.insert(rights);
                if statement.grant_option {
                    next[index].grant_options.insert(rights);
                }
            } else if let Ok(index) = pos {
                next[index].grant_options.remove(rights);
                if !statement.grant_option {
                    next[index].privileges.remove(rights);
                }
            }
        }
        next.retain(|entry| !entry.privileges.is_empty());
        Ok((next != baseline).then_some(next))
    }

    pub fn after_drop(
        self,
        mut entries: Entries,
        roles: &std::collections::BTreeSet<RoleIdentity>,
    ) -> Option<Entries> {
        entries.retain(|entry| !entry.role.is_some_and(|role| roles.contains(&role)));
        (entries != self.baseline()).then_some(entries)
    }
}

impl DefaultAcls {
    /// Compose the owner's global ACL with additions for this namespace. An absent result keeps the ordinary object's native default representation.
    pub fn initial(
        &self,
        owner: RoleIdentity,
        namespace: u32,
        object: DefaultAclObject,
    ) -> Option<Entries> {
        let global = DefaultAclKey {
            owner,
            namespace: 0,
            object,
        };
        let base = self.get(&global);
        let local = (namespace != 0)
            .then(|| {
                self.get(&DefaultAclKey {
                    namespace,
                    ..global
                })
            })
            .flatten();
        if base.is_none() && local.is_none() {
            return None;
        }
        let baseline = global.baseline();
        let mut entries = base.map_or_else(|| baseline.clone(), |row| row.acl.clone());
        if let Some(local) = local {
            for entry in &local.acl {
                match entries.binary_search_by_key(&entry.role, |item| item.role) {
                    Ok(index) => {
                        entries[index].privileges.insert(entry.privileges);
                        entries[index].grant_options.insert(entry.grant_options);
                    }
                    Err(index) => entries.insert(index, entry.clone()),
                }
            }
        }
        (entries != baseline).then_some(entries)
    }
}

/// Creation reads one committed catalog generation under the object's definition transaction.
pub trait InitialPrivileges {
    fn initial_acl(
        &self,
        owner: RoleIdentity,
        namespace: Option<&str>,
        object: DefaultAclObject,
    ) -> Option<Entries>;

    fn table_security(&self, owner: RoleIdentity, namespace: &str) -> super::BoundTableSecurity {
        let mut security = super::BoundTableSecurity::owner(owner);
        security.acl = self
            .initial_acl(owner, Some(namespace), DefaultAclObject::Table)
            .map(table);
        security
    }
    fn type_acl(&self, owner: RoleIdentity, namespace: &str) -> Option<Vec<ObjectAclEntry>> {
        self.initial_acl(owner, Some(namespace), DefaultAclObject::Type)
            .map(single)
    }
}

pub fn table(entries: Entries) -> Vec<BoundAclEntry<TablePrivileges>> {
    convert(entries, DefaultRights::table)
}
pub fn sequence(entries: Entries) -> Vec<BoundAclEntry<SequencePrivileges>> {
    convert(entries, DefaultRights::sequence)
}
pub fn schema(entries: Entries) -> Vec<BoundAclEntry<SchemaPrivileges>> {
    convert(entries, DefaultRights::schema)
}

fn convert<P>(entries: Entries, rights: fn(DefaultRights) -> P) -> Vec<BoundAclEntry<P>> {
    entries
        .into_iter()
        .map(|entry| BoundAclEntry {
            role: entry.role,
            grantor: entry.grantor,
            privileges: rights(entry.privileges),
            grant_options: rights(entry.grant_options),
        })
        .collect()
}

pub fn single(entries: Entries) -> Vec<ObjectAclEntry> {
    entries
        .into_iter()
        .map(|entry| ObjectAclEntry {
            role: entry.role,
            grantor: entry.grantor,
            grant_option: !entry.grant_options.is_empty(),
        })
        .collect()
}

fn invalid_grant(message: impl Into<String>) -> SQLError {
    SQLError::Routine {
        sqlstate: "0LP01".into(),
        message: message.into(),
    }
}
