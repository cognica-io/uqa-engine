//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! A definition mutation retains its originally selected catalog tuple, independently of role authority.

use super::{
    identity::{RoleBinding, RoleSubject},
    RoleDefinition,
};
use crate::SQLError;
use std::collections::BTreeMap;

#[derive(Clone, Debug)]
pub struct RoleTuple {
    pub role: RoleBinding,
    pub revision: u64,
}

impl RoleTuple {
    pub fn bind(definition: &RoleDefinition) -> Result<Self, SQLError> {
        if definition.revision == 0 {
            return Err(SQLError::Internal(
                "role definition has no tuple revision".into(),
            ));
        }
        Ok(Self {
            role: RoleBinding::from_definition(definition)?,
            revision: definition.revision,
        })
    }

    pub fn revalidate<'a>(
        &self,
        roles: &'a BTreeMap<String, RoleDefinition>,
    ) -> Result<&'a RoleDefinition, SQLError> {
        let current = self
            .role
            .role_definition(roles)
            .ok_or_else(|| SQLError::Routine {
                sqlstate: "XX000".into(),
                message: "tuple concurrently deleted".into(),
            })?;
        if current.revision != self.revision {
            return Err(SQLError::Routine {
                sqlstate: "XX000".into(),
                message: "tuple concurrently updated".into(),
            });
        }
        Ok(current)
    }
}

pub fn validate_revisions(roles: &BTreeMap<String, RoleDefinition>) -> Result<(), String> {
    for (name, role) in roles {
        if role.revision == 0 {
            return Err(format!("persisted role `{name}` has no tuple revision"));
        }
    }
    Ok(())
}

pub fn validate_revision_changes(
    before: &BTreeMap<String, RoleDefinition>,
    after: &BTreeMap<String, RoleDefinition>,
) -> Result<(), SQLError> {
    validate_revisions(before).map_err(SQLError::Internal)?;
    validate_revisions(after).map_err(SQLError::Internal)?;
    for (name, role) in after {
        let previous = before
            .get(name)
            .filter(|previous| previous.identity() == role.identity())
            .or_else(|| {
                before
                    .values()
                    .find(|previous| previous.identity() == role.identity())
            });
        let valid = previous.map_or(role.revision == 1, |previous| {
            previous == role || previous.revision.checked_add(1) == Some(role.revision)
        });
        if !valid {
            return Err(SQLError::Internal(format!(
                "role `{name}` did not advance its tuple revision"
            )));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
