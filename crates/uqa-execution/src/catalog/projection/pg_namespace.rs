//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Capability-scoped `pg_namespace` row synthesis.

use crate::catalog::{CatalogReadView, RelationNameResolution};
use uqa_sql::{ResultRow, SQLError};

use super::helpers::acl::acl_identifier;
use super::helpers::oids::{current_user_oid, namespace_oid};
use super::helpers::rows::{catalog_array, int_value, row, str_value};

pub fn build_pg_namespace(
    catalog: &CatalogReadView,
    resolution: &RelationNameResolution,
) -> Result<Vec<ResultRow>, SQLError> {
    catalog
        .all_schema_names(resolution)
        .into_iter()
        .map(|schema| {
            let security = catalog.schema_security(&schema);
            let names = catalog.schema_security_names(&schema)?;
            Ok(row([
                ("oid", int_value(namespace_oid(catalog, &schema))),
                ("nspname", str_value(&schema)),
                (
                    "nspowner",
                    int_value(match security {
                        Some(security) => security.role_owner.oid,
                        None => current_user_oid(),
                    }),
                ),
                ("nspacl", schema_acl_catalog_value(names.as_ref())?),
            ]))
        })
        .collect::<Result<Vec<_>, SQLError>>()
}

fn schema_acl_catalog_value(
    security: Option<&crate::catalog::security::SchemaSecurity>,
) -> Result<uqa_core::Value, SQLError> {
    let Some(security) = security else {
        return Ok(uqa_core::Value::Null);
    };
    let Some(acl) = security.acl.as_ref() else {
        return Ok(uqa_core::Value::Null);
    };
    catalog_array(
        acl.iter()
            .map(|entry| {
                let grantee = entry
                    .role
                    .role_name()
                    .map_or_else(String::new, acl_identifier);
                let grantor =
                    acl_identifier(entry.grantor.as_deref().unwrap_or(&security.role_owner));
                let mut privileges = String::new();
                for (enabled, grant_option, code) in [
                    (entry.privileges.usage, entry.grant_options.usage, 'U'),
                    (entry.privileges.create, entry.grant_options.create, 'C'),
                ] {
                    if enabled {
                        privileges.push(code);
                        if grant_option {
                            privileges.push('*');
                        }
                    }
                }
                str_value(format!("{grantee}={privileges}/{grantor}"))
            })
            .collect(),
        "pg_namespace.nspacl",
    )
}

#[cfg(test)]
mod tests;
