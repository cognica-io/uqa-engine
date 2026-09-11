//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Role ownership and ACL dependency diagnostics in catalog and requested-role order.

use crate::{
    catalog::{
        security::{database::DatabaseSecurity, SchemaSecurity, SequenceSecurity, TableSecurity},
        view::StoredViewKind,
    },
    SQLError,
};
use std::collections::BTreeMap;
use uqa_core::RelationIdentity;
pub mod context;
use context::RoleDependencyCatalog;

pub fn ensure_roles_have_no_object_dependencies(
    catalog: &dyn RoleDependencyCatalog,
    names: &[String],
) -> Result<(), SQLError> {
    let database_security = catalog.database();
    for name in names {
        if database_depends_on_role(&database_security, name) {
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
        if let Some(schema) = dependent_schema_for_role(&schema_security, name) {
            return Err(SQLError::Routine {
                sqlstate: "2BP01".into(),
                message: format!(
                    "role \"{name}\" cannot be dropped because some objects depend on it: schema {schema}"
                ),
            });
        }
    }
    drop(schema_security);

    let tables = catalog.tables();
    for name in names {
        if let Some(relation) = tables.iter().find_map(|(relation, table)| {
            table_security_depends_on_role(&table.security(), name).then_some(relation)
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

    let views = catalog.views();
    for name in names {
        if let Some((relation, view)) = views
            .iter()
            .find(|(_, view)| table_security_depends_on_role(&view.security(), name))
        {
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

    ensure_roles_have_no_foreign_table_dependencies(catalog, names)?;

    let sequence_security = catalog.sequences();
    for name in names {
        if let Some(relation) = dependent_sequence_for_role(&sequence_security, name) {
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

    let routines = catalog.routines();
    for name in names {
        if let Some(dependent) = routines.values().flatten().find_map(|function| {
            let owns = function.def.owner == *name;
            let has_acl = function.def.execute_acl.as_ref().is_some_and(|acl| {
                acl.iter().any(|entry| {
                    entry.role == *name
                        || entry.grantor.as_deref().unwrap_or(&function.def.owner) == name
                })
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

fn ensure_roles_have_no_foreign_table_dependencies(
    catalog: &dyn RoleDependencyCatalog,
    names: &[String],
) -> Result<(), SQLError> {
    let foreign_tables = catalog.foreign_tables();
    for name in names {
        if let Some((relation, _)) = foreign_tables
            .iter()
            .find(|(_, security)| table_security_depends_on_role(security, name))
        {
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

fn table_security_depends_on_role(security: &TableSecurity, role: &str) -> bool {
    let acl_dependency = security.acl.as_ref().is_some_and(|acl| {
        acl.iter().any(|entry| {
            entry.role == role || entry.grantor.as_deref().unwrap_or(&security.role_owner) == role
        })
    });
    let column_acl_dependency = security.column_acls.values().any(|acl| {
        acl.iter().any(|entry| {
            entry.role == role || entry.grantor.as_deref().unwrap_or(&security.role_owner) == role
        })
    });
    security.role_owner == role || acl_dependency || column_acl_dependency
}

fn dependent_sequence_for_role<'a>(
    sequences: &'a BTreeMap<RelationIdentity, SequenceSecurity>,
    role: &str,
) -> Option<&'a RelationIdentity> {
    sequences.iter().find_map(|(relation, security)| {
        let acl_dependency = security.acl.as_ref().is_some_and(|acl| {
            acl.iter().any(|entry| {
                entry.role == role
                    || entry.grantor.as_deref().unwrap_or(&security.role_owner) == role
            })
        });
        (security.role_owner == role || acl_dependency).then_some(relation)
    })
}

fn dependent_schema_for_role<'a>(
    schemas: &'a BTreeMap<String, SchemaSecurity>,
    role: &str,
) -> Option<&'a String> {
    schemas.iter().find_map(|(name, security)| {
        let acl_dependency = security.acl.as_ref().is_some_and(|acl| {
            acl.iter().any(|entry| {
                entry.role == role
                    || entry.grantor.as_deref().unwrap_or(&security.role_owner) == role
            })
        });
        (security.role_owner == role || acl_dependency).then_some(name)
    })
}

fn database_depends_on_role(security: &DatabaseSecurity, role: &str) -> bool {
    let acl_dependency = security.acl.as_ref().is_some_and(|acl| {
        acl.iter().any(|entry| {
            entry.role == role || entry.grantor.as_deref().unwrap_or(&security.role_owner) == role
        })
    });
    security.role_owner == role || acl_dependency
}

#[cfg(test)]
mod tests;
