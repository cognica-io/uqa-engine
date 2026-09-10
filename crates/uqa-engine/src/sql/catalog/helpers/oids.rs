//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Stable catalog identity and OID policy.

use crate::RelationIdentity;
use uqa_sql::SQLError;

pub(in crate::sql::catalog) fn split_schema_name(name: &str) -> Result<(String, String), SQLError> {
    let relation = RelationIdentity::from_legacy_name(name).map_err(|error| {
        SQLError::Internal(format!("invalid catalog relation `{name}`: {error}"))
    })?;
    Ok((relation.schema, relation.name))
}

pub(in crate::sql::catalog) use uqa_sql::catalog::oids::{
    current_user_name, current_user_oid, relation_oid, schema_oid, stable_object_oid, stable_oid,
};
