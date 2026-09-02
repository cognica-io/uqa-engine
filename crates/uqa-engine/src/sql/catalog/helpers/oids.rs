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

pub(in crate::sql::catalog) fn stable_oid(kind: &str, name: &str) -> i64 {
    let mut hash = 14_695_981_039_346_656_037_u64;
    for byte in kind.bytes().chain(*b":").chain(name.bytes()) {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(1_099_511_628_211);
    }
    10_000 + i64::try_from(hash % 2_000_000_000).unwrap_or(0)
}

pub(in crate::sql::catalog) fn stable_object_oid(kind: &str, object_id: &[u8; 16]) -> i64 {
    let mut hash = 14_695_981_039_346_656_037_u64;
    for byte in kind.bytes().chain(*b":").chain(object_id.iter().copied()) {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(1_099_511_628_211);
    }
    10_000 + i64::try_from(hash % 2_000_000_000).unwrap_or(0)
}

pub(in crate::sql::catalog) fn schema_oid(schema: &str) -> i64 {
    match schema {
        "pg_catalog" => 11,
        "public" => 2200,
        "information_schema" => 13_293,
        other => stable_oid("namespace", other),
    }
}

pub(in crate::sql::catalog) fn relation_oid(kind: &str, schema: &str, name: &str) -> i64 {
    stable_oid(kind, &format!("{schema}.{name}"))
}

pub(in crate::sql::catalog) fn current_user_oid() -> i64 {
    10
}

pub(in crate::sql::catalog) fn current_user_name() -> &'static str {
    "uqa"
}
