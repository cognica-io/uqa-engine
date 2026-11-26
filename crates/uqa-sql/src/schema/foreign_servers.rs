//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `PostgreSQL` foreign-server option changes and owner privileges.

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
            return Err(duplicate_option(name));
        }
    }
    validate_names(options.iter().map(|(name, _)| name.as_str()))?;
    Ok(result)
}

/// Read server and table options in their stored declaration order, using key order for older map-only records.
pub fn ordered_option_pairs<'a>(
    options: &'a uqa_core::memory::OwnedMap<String, String>,
    order: Option<&'a [String]>,
) -> impl Iterator<Item = Result<(&'a str, &'a str), SQLError>> {
    let written = order.into_iter().flatten().map(move |name| {
        options
            .get(name)
            .map(|value| (name.as_str(), value.as_str()))
            .ok_or_else(|| {
                SQLError::Internal("foreign option order references a missing key".into())
            })
    });
    let legacy = order
        .is_none()
        .then(|| options.iter())
        .into_iter()
        .flatten()
        .map(|(name, value)| Ok((name.as_str(), value.as_str())));
    written.chain(legacy)
}

fn duplicate_option(name: &str) -> SQLError {
    SQLError::Routine {
        sqlstate: "42710".into(),
        message: format!("option \"{name}\" provided more than once"),
    }
}

fn missing_option(name: &str) -> SQLError {
    SQLError::Routine {
        sqlstate: "42704".into(),
        message: format!("option \"{name}\" not found"),
    }
}

fn validate_names<'a>(mut names: impl Iterator<Item = &'a str>) -> Result<(), SQLError> {
    if let Some(name) = names.find(|name| name.contains('=')) {
        return Err(SQLError::Routine {
            sqlstate: "22023".into(),
            message: format!("invalid option name \"{name}\": must not contain \"=\""),
        });
    }
    Ok(())
}

/// Apply each action to the preceding action's result. SET keeps its slot and a later ADD takes a new slot.
pub fn altered_options(
    server: &ForeignServerDefinition,
    changes: &[crate::ast::ForeignOptionChange],
) -> Result<Vec<(String, String)>, SQLError> {
    use crate::ast::ForeignOptionChange::{Add, Drop, Set};
    use std::collections::btree_map::Entry;
    let current = ordered_option_pairs(&server.options, server.metadata.option_order.as_deref());
    let mut slots: BTreeMap<_, _> = current
        .enumerate()
        .map(|(position, pair)| pair.map(|(name, value)| (name, (position, value))))
        .collect::<Result<_, _>>()?;
    let mut next_position = slots.len();
    for change in changes {
        match change {
            Add { name, value } => match slots.entry(name.as_str()) {
                Entry::Vacant(entry) => {
                    entry.insert((next_position, value.as_str()));
                    next_position += 1;
                }
                Entry::Occupied(_) => return Err(duplicate_option(name)),
            },
            Set { name, value } => {
                slots
                    .get_mut(name.as_str())
                    .ok_or_else(|| missing_option(name))?
                    .1 = value;
            }
            Drop { name } => {
                slots
                    .remove(name.as_str())
                    .ok_or_else(|| missing_option(name))?;
            }
        }
    }
    let mut ordered: Vec<_> = slots.into_iter().collect();
    ordered.sort_unstable_by_key(|(_, (position, _))| *position);
    validate_names(ordered.iter().map(|(name, _)| *name))?;
    Ok(ordered
        .into_iter()
        .map(|(name, (_, value))| (name.to_owned(), value.to_owned()))
        .collect())
}

/// Check the current role against the selected server's exact owner incarnation.
pub fn require_owner(
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

pub use require_owner as ensure_drop_authority;

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
