//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `PostgreSQL` foreign-server declaration options and deletion authority.

use crate::{
    catalog::{
        foreign_server::ForeignServerDefinition,
        roles::{guards::RoleCatalogGuards, role_inherits, RoleReferenceNames},
    },
    SQLError, SQLNotice,
};
use std::collections::BTreeMap;

/// `transformGenericOptions` checks duplicate names before `optionListToArray` rejects an equals sign in a name.
pub fn creation_options(
    options: &[(String, String)],
) -> Result<BTreeMap<String, String>, SQLError> {
    let mut result = BTreeMap::new();
    for (name, value) in options {
        if result.insert(name.clone(), value.clone()).is_some() {
            return Err(SQLError::Routine {
                sqlstate: "42710".into(),
                message: format!("option \"{name}\" provided more than once"),
            });
        }
    }
    if let Some((name, _)) = options.iter().find(|(name, _)| name.contains('=')) {
        return Err(SQLError::Routine {
            sqlstate: "22023".into(),
            message: format!("invalid option name \"{name}\": must not contain \"=\""),
        });
    }
    Ok(result)
}

/// Check the current role against the selected server's exact owner incarnation. Execution repeats this check on the freshly resolved definition after acquiring its object lock.
pub fn ensure_drop_authority(
    server: &ForeignServerDefinition,
    session: &dyn RoleReferenceNames,
    catalog: &dyn RoleCatalogGuards,
) -> Result<(), SQLError> {
    let current = session.current_role();
    let roles = catalog.role_definitions();
    let memberships = catalog.role_memberships();
    if role_inherits(&roles, &memberships, &current, &server.metadata.owner) {
        return Ok(());
    }
    Err(SQLError::Routine {
        sqlstate: "42501".into(),
        message: format!("must be owner of foreign server {}", server.name),
    })
}

pub fn missing_server(name: &str) -> SQLError {
    SQLError::Routine {
        sqlstate: "42704".into(),
        message: format!("server \"{name}\" does not exist"),
    }
}

pub fn missing_server_notice(name: &str) -> SQLNotice {
    SQLNotice::notice(format!("server \"{name}\" does not exist, skipping"))
}

#[cfg(test)]
mod tests;
