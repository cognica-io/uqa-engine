//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Stable catalog identity and OID policy.

use uqa_core::RelationIdentity;
use uqa_sql::SQLError;

/// Resolve namespace references from the same catalog snapshot as their owning rows. A graph's schema and the session's temporary namespaces have the OIDs their creation allocated.
pub fn namespace_oid(catalog: &crate::catalog::CatalogReadView, name: &str) -> i64 {
    if let Some(oid) = catalog
        .snapshot()
        .temporary_namespace
        .as_ref()
        .and_then(|temporary| temporary.namespace_oid(name))
    {
        return i64::from(oid);
    }
    if let Some(oids) = catalog.graph_catalog_oids(name) {
        return i64::from(oids.namespace);
    }
    catalog
        .schema_security(name)
        .map_or_else(|| schema_oid(name), |security| security.namespace_oid(name))
}

pub fn split_schema_name(name: &str) -> Result<(String, String), SQLError> {
    let relation = RelationIdentity::from_legacy_name(name).map_err(|error| {
        SQLError::Internal(format!("invalid catalog relation `{name}`: {error}"))
    })?;
    Ok((relation.schema, relation.name))
}

pub use uqa_sql::catalog::oids::{
    current_user_name, current_user_oid, relation_oid, schema_oid, stable_object_oid, stable_oid,
};
