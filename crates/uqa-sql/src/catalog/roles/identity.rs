//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Role incarnations and authorization subjects over an explicit role catalog.

use super::RoleDefinition;
use crate::SQLError;
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, sync::Arc};
pub use uqa_core::catalog_role::RoleIdentity;

/// A selected role keeps its incarnation even if another role later reuses its name or OID.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoleBinding {
    pub name: String,
    pub oid: u32,
    pub object_id: [u8; 16],
}

/// Explicit SQL names are resolved against the current catalog; captured authority retains its incarnation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum RoleReference {
    Named(String),
    Bound(Arc<RoleBinding>),
}

impl From<String> for RoleReference {
    fn from(name: String) -> Self {
        Self::Named(name)
    }
}

impl From<&str> for RoleReference {
    fn from(name: &str) -> Self {
        Self::Named(name.into())
    }
}

impl RoleReference {
    pub fn require_name<'a>(
        &'a self,
        roles: &'a BTreeMap<String, RoleDefinition>,
    ) -> Result<&'a str, SQLError> {
        match self {
            Self::Named(name) => {
                super::require_role_exists(roles, name)?;
                Ok(name)
            }
            Self::Bound(role) => role.require_name(roles),
        }
    }

    pub fn catalog_name(
        &self,
        roles: &BTreeMap<String, RoleDefinition>,
    ) -> Result<String, SQLError> {
        match self {
            Self::Named(name) => Ok(name.clone()),
            Self::Bound(role) => role.require_name(roles).map(str::to_owned),
        }
    }

    pub fn bind(&self, roles: &BTreeMap<String, RoleDefinition>) -> Result<RoleBinding, SQLError> {
        let name = self.require_name(roles)?;
        RoleBinding::from_definition(&roles[name])
    }
}

impl RoleSubject for RoleReference {
    fn role_name<'a>(&'a self, roles: &'a BTreeMap<String, RoleDefinition>) -> Option<&'a str> {
        match self {
            Self::Named(name) => name.role_name(roles),
            Self::Bound(role) => role.role_name(roles),
        }
    }

    fn role_definition<'a>(
        &self,
        roles: &'a BTreeMap<String, RoleDefinition>,
    ) -> Option<&'a RoleDefinition> {
        match self {
            Self::Named(name) => name.role_definition(roles),
            Self::Bound(role) => role.role_definition(roles),
        }
    }
}

impl RoleBinding {
    pub fn identity(&self) -> RoleIdentity {
        RoleIdentity {
            oid: i64::from(self.oid),
            object_id: self.object_id,
        }
    }

    pub fn from_definition(role: &RoleDefinition) -> Result<Self, SQLError> {
        if role.oid <= 0 {
            return Err(SQLError::Internal("invalid role OID".into()));
        }
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

    /// Validate the original name and identity before publishing a stored role reference.
    pub fn revalidate(&self, roles: &BTreeMap<String, RoleDefinition>) -> Result<(), SQLError> {
        if roles
            .get(&self.name)
            .is_some_and(|role| self.matches_definition(role))
        {
            Ok(())
        } else {
            Err(SQLError::Routine {
                sqlstate: "42704".into(),
                message: format!("role {} was concurrently dropped", self.oid),
            })
        }
    }

    fn matches_definition(&self, role: &RoleDefinition) -> bool {
        role.oid == i64::from(self.oid) && role.object_id == self.object_id
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

impl<T: RoleSubject + ?Sized> RoleSubject for Arc<T> {
    fn role_name<'a>(&'a self, roles: &'a BTreeMap<String, RoleDefinition>) -> Option<&'a str> {
        self.as_ref().role_name(roles)
    }
    fn role_definition<'a>(
        &self,
        roles: &'a BTreeMap<String, RoleDefinition>,
    ) -> Option<&'a RoleDefinition> {
        self.as_ref().role_definition(roles)
    }
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
        roles
            .get(&self.name)
            .filter(|role| self.matches_definition(role))
            .or_else(|| roles.values().find(|role| self.matches_definition(role)))
    }
}

impl RoleSubject for RoleIdentity {
    fn role_name<'a>(&'a self, roles: &'a BTreeMap<String, RoleDefinition>) -> Option<&'a str> {
        self.role_definition(roles).map(|role| role.name.as_str())
    }

    fn role_definition<'a>(
        &self,
        roles: &'a BTreeMap<String, RoleDefinition>,
    ) -> Option<&'a RoleDefinition> {
        if !self.is_valid() {
            return None;
        }
        roles.values().find(|role| role.identity() == *self)
    }
}

#[cfg(test)]
mod tests;
