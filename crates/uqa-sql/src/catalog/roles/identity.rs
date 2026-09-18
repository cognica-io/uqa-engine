//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Role incarnations and authorization subjects over an explicit role catalog.

use super::RoleDefinition;
use crate::SQLError;
use std::collections::BTreeMap;

/// A selected role keeps its incarnation even if another role later reuses its name or OID.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RoleBinding {
    pub name: String,
    pub oid: u32,
    pub object_id: [u8; 16],
}

impl RoleBinding {
    pub fn from_definition(role: &RoleDefinition) -> Result<Self, SQLError> {
        if role.object_id == [0; 16] {
            return Err(SQLError::Internal("role has no object identity".into()));
        }
        Ok(Self {
            name: role.name.clone(),
            oid: u32::try_from(role.oid)
                .map_err(|_| SQLError::Internal("invalid role OID".into()))?,
            object_id: role.object_id,
        })
    }

    pub fn revalidate(&self, roles: &BTreeMap<String, RoleDefinition>) -> Result<(), SQLError> {
        if self.role_definition(roles).is_some() {
            Ok(())
        } else {
            Err(SQLError::Routine {
                sqlstate: "42704".into(),
                message: format!("role {} was concurrently dropped", self.oid),
            })
        }
    }

    pub fn require_name<'a>(
        &self,
        roles: &'a BTreeMap<String, RoleDefinition>,
    ) -> Result<&'a str, SQLError> {
        self.role_definition(roles)
            .map(|role| role.name.as_str())
            .ok_or_else(|| SQLError::Routine {
                sqlstate: "42704".into(),
                message: format!("invalid role OID: {}", self.oid),
            })
    }
}

/// A named SQL argument and an already selected session role have different lookup semantics. An absent bound role contributes no inherited privileges; PUBLIC ACLs remain applicable.
pub trait RoleSubject {
    fn role_name<'a>(&'a self, roles: &'a BTreeMap<String, RoleDefinition>) -> Option<&'a str>;

    fn role_definition<'a>(
        &self,
        roles: &'a BTreeMap<String, RoleDefinition>,
    ) -> Option<&'a RoleDefinition>;
}

impl<T: RoleSubject + ?Sized> RoleSubject for &T {
    fn role_name<'a>(&'a self, roles: &'a BTreeMap<String, RoleDefinition>) -> Option<&'a str> {
        (*self).role_name(roles)
    }

    fn role_definition<'a>(
        &self,
        roles: &'a BTreeMap<String, RoleDefinition>,
    ) -> Option<&'a RoleDefinition> {
        (*self).role_definition(roles)
    }
}

impl RoleSubject for str {
    fn role_name<'a>(&'a self, _roles: &'a BTreeMap<String, RoleDefinition>) -> Option<&'a str> {
        Some(self)
    }

    fn role_definition<'a>(
        &self,
        roles: &'a BTreeMap<String, RoleDefinition>,
    ) -> Option<&'a RoleDefinition> {
        roles.get(self)
    }
}

impl RoleSubject for String {
    fn role_name<'a>(&'a self, roles: &'a BTreeMap<String, RoleDefinition>) -> Option<&'a str> {
        self.as_str().role_name(roles)
    }

    fn role_definition<'a>(
        &self,
        roles: &'a BTreeMap<String, RoleDefinition>,
    ) -> Option<&'a RoleDefinition> {
        self.as_str().role_definition(roles)
    }
}

impl RoleSubject for RoleBinding {
    fn role_name<'a>(&'a self, roles: &'a BTreeMap<String, RoleDefinition>) -> Option<&'a str> {
        self.role_definition(roles).map(|role| role.name.as_str())
    }

    fn role_definition<'a>(
        &self,
        roles: &'a BTreeMap<String, RoleDefinition>,
    ) -> Option<&'a RoleDefinition> {
        let matches = |role: &&RoleDefinition| {
            role.oid == i64::from(self.oid) && role.object_id == self.object_id
        };
        roles
            .get(&self.name)
            .filter(matches)
            .or_else(|| roles.values().find(matches))
    }
}

#[cfg(test)]
mod tests;
