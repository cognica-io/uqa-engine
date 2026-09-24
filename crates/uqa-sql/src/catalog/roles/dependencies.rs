//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Role ownership and ACL dependency diagnostics in catalog and requested-role order.

use crate::{
    catalog::{
        security::{BoundSchemaSecurity, BoundSequenceSecurity},
        view::StoredViewKind,
    },
    SQLError,
};
use std::collections::BTreeMap;
use uqa_core::RelationIdentity;
pub mod context;
pub mod temporary;
use context::RoleDependencyCatalog;

pub fn ensure_roles_have_no_object_dependencies(
    catalog: &dyn RoleDependencyCatalog,
    names: &[String],
    roles: &BTreeMap<String, super::RoleDefinition>,
) -> Result<(), SQLError> {
    let database_security = catalog.database();
    for name in names {
        if roles
            .get(name)
            .is_some_and(|role| database_security.depends_on(role.identity()))
        {
            return Err(SQLError::Routine {
                sqlstate: "2BP01".into(),
                message: format!(
                    "role \"{name}\" cannot be dropped because some objects depend on it: database uqa"
                ),
            });
        }
    }
    drop(database_security);

    let schema_security = catalog.schemas();
    for name in names {
        if let Some(schema) = roles
            .get(name)
            .and_then(|role| dependent_schema_for_role(&schema_security, role.identity()))
        {
            return Err(SQLError::Routine {
                sqlstate: "2BP01".into(),
                message: format!(
                    "role \"{name}\" cannot be dropped because some objects depend on it: schema {schema}"
                ),
            });
        }
    }
    drop(schema_security);

    ensure_roles_have_no_table_dependencies(catalog, names, roles)?;

    ensure_roles_have_no_system_relation_dependencies(catalog, names, roles)?;

    let views = catalog.views();
    for name in names {
        if let Some((relation, view)) = views.iter().find(|(_, view)| {
            roles
                .get(name)
                .is_some_and(|role| view.security.depends_on(role.identity()))
        }) {
            let kind = match view.kind {
                StoredViewKind::View => "view",
                StoredViewKind::Materialized => "materialized view",
            };
            return Err(SQLError::Routine {
                sqlstate: "2BP01".into(),
                message: format!(
                    "role \"{name}\" cannot be dropped because some objects depend on it: {kind} {}",
                    relation.qualified_name()
                ),
            });
        }
    }
    drop(views);

    ensure_roles_have_no_foreign_table_dependencies(catalog, names, roles)?;

    let sequence_security = catalog.sequences();
    for name in names {
        if let Some(relation) = roles
            .get(name)
            .and_then(|role| dependent_sequence_for_role(&sequence_security, role.identity()))
        {
            return Err(SQLError::Routine {
                sqlstate: "2BP01".into(),
                message: format!(
                    "role \"{name}\" cannot be dropped because some objects depend on it: sequence {}",
                    relation.qualified_name()
                ),
            });
        }
    }
    drop(sequence_security);

    ensure_roles_have_no_domain_dependencies(catalog, names, roles)?;

    let routines = catalog.routines();
    for name in names {
        if let Some(dependent) = routines.values().flatten().find_map(|function| {
            let identity = roles.get(name)?.identity();
            let owns = function.def.owner == Some(identity);
            let has_acl = function.def.execute_acl.as_ref().is_some_and(|acl| {
                acl.iter()
                    .any(|entry| entry.role == Some(identity) || entry.grantor == identity)
            });
            (owns || has_acl).then(|| format!("routine {}", function.def.name))
        }) {
            return Err(SQLError::Routine {
                sqlstate: "2BP01".into(),
                message: format!("role \"{name}\" cannot be dropped because some objects depend on it: {dependent}"),
            });
        }
    }
    Ok(())
}

fn ensure_roles_have_no_domain_dependencies(
    catalog: &dyn RoleDependencyCatalog,
    names: &[String],
    roles: &BTreeMap<String, super::RoleDefinition>,
) -> Result<(), SQLError> {
    let domains = catalog.domains();
    for name in names {
        if let Some(domain) = domains.values().find(|domain| {
            roles
                .get(name)
                .is_some_and(|role| domain.owner == role.identity())
        }) {
            return Err(SQLError::Routine {
                sqlstate: "2BP01".into(),
                message: format!(
                    "role \"{name}\" cannot be dropped because some objects depend on it: type {}",
                    domain.identity.qualified_name()
                ),
            });
        }
    }
    drop(domains);
    Ok(())
}

fn ensure_roles_have_no_table_dependencies(
    catalog: &dyn RoleDependencyCatalog,
    names: &[String],
    roles: &BTreeMap<String, super::RoleDefinition>,
) -> Result<(), SQLError> {
    let tables = catalog.tables();
    for name in names {
        if let Some(relation) = tables.iter().find_map(|(relation, table)| {
            roles
                .get(name)
                .is_some_and(|role| table.security().depends_on(role.identity()))
                .then_some(relation)
        }) {
            return Err(SQLError::Routine {
                sqlstate: "2BP01".into(),
                message: format!(
                    "role \"{name}\" cannot be dropped because some objects depend on it: table {}",
                    relation.qualified_name()
                ),
            });
        }
    }
    drop(tables);

    Ok(())
}

fn ensure_roles_have_no_system_relation_dependencies(
    catalog: &dyn RoleDependencyCatalog,
    names: &[String],
    roles: &BTreeMap<String, super::RoleDefinition>,
) -> Result<(), SQLError> {
    let system_relations = catalog.system_relation_securities();
    for name in names {
        if let Some((relation, _)) = system_relations.iter().find(|(identity, entry)| {
            crate::catalog::SystemRelation::at(&identity.schema, &identity.name).is_some_and(
                |relation| {
                    roles
                        .get(name)
                        .is_some_and(|role| entry.security(relation).depends_on(role.identity()))
                },
            )
        }) {
            return Err(SQLError::Routine {
                sqlstate: "2BP01".into(),
                message: format!(
                    "role \"{name}\" cannot be dropped because some objects depend on it: table {}",
                    relation.qualified_name()
                ),
            });
        }
    }
    drop(system_relations);

    Ok(())
}

fn ensure_roles_have_no_foreign_table_dependencies(
    catalog: &dyn RoleDependencyCatalog,
    names: &[String],
    roles: &BTreeMap<String, super::RoleDefinition>,
) -> Result<(), SQLError> {
    let foreign_tables = catalog.foreign_tables();
    for name in names {
        if let Some((relation, _)) = foreign_tables.iter().find(|(_, security)| {
            roles
                .get(name)
                .is_some_and(|role| security.depends_on(role.identity()))
        }) {
            return Err(SQLError::Routine {
                sqlstate: "2BP01".into(),
                message: format!(
                    "role \"{name}\" cannot be dropped because some objects depend on it: foreign table {}",
                    relation.qualified_name()
                ),
            });
        }
    }
    Ok(())
}

fn dependent_sequence_for_role(
    sequences: &BTreeMap<RelationIdentity, BoundSequenceSecurity>,
    role: super::RoleIdentity,
) -> Option<&RelationIdentity> {
    sequences
        .iter()
        .find_map(|(relation, security)| security.depends_on(role).then_some(relation))
}

fn dependent_schema_for_role(
    schemas: &BTreeMap<String, BoundSchemaSecurity>,
    role: super::RoleIdentity,
) -> Option<&String> {
    schemas
        .iter()
        .find_map(|(name, security)| security.depends_on(role).then_some(name))
}

#[cfg(test)]
mod tests;
