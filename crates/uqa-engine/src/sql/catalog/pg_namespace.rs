//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Capability-scoped `pg_namespace` row synthesis.

use crate::engine_capabilities::{CatalogReadView, RelationNameResolution};
use uqa_sql::{ResultRow, SQLError};

use super::helpers::acl::acl_identifier;
use super::helpers::oids::{current_user_oid, schema_oid};
use super::helpers::rows::{catalog_array, int_value, row, str_value};
use super::helpers::views::all_schema_names;

pub(super) fn build_pg_namespace(
    catalog: &CatalogReadView,
    resolution: &RelationNameResolution,
) -> Result<Vec<ResultRow>, SQLError> {
    all_schema_names(catalog, resolution)?
        .into_iter()
        .map(|schema| {
            let security = catalog.schema_security(&schema);
            Ok(row([
                ("oid", int_value(schema_oid(&schema))),
                ("nspname", str_value(&schema)),
                (
                    "nspowner",
                    int_value(security.map_or_else(current_user_oid, |security| {
                        crate::engine_roles::role_oid(&security.role_owner)
                    })),
                ),
                ("nspacl", schema_acl_catalog_value(security)?),
            ]))
        })
        .collect::<Result<Vec<_>, SQLError>>()
}

fn schema_acl_catalog_value(
    security: Option<&crate::engine_state::SchemaSecurity>,
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
                let grantee = if entry.role == "PUBLIC" {
                    String::new()
                } else {
                    acl_identifier(&entry.role)
                };
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
