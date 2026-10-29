//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Stable identities for SQL catalog objects.

/// The first OID a user-created object receives (`FirstNormalObjectId`); lower OIDs belong to the system catalogs.
pub const FIRST_NORMAL_OBJECT_ID: u32 = 16_384;

pub fn stable_oid(kind: &str, name: &str) -> i64 {
    stable_oid_bytes(kind, name.bytes())
}

/// Decimal OID identities use the same bytes as textual formatting without allocating temporary text.
pub fn stable_numeric_oid(kind: &str, mut name: u32) -> i64 {
    let mut digits = [0_u8; 10];
    let mut start = digits.len();
    loop {
        start -= 1;
        digits[start] = b'0' + (name % 10) as u8;
        name /= 10;
        if name == 0 {
            break;
        }
    }
    stable_oid_bytes(kind, digits[start..].iter().copied())
}

pub fn stable_object_oid(kind: &str, object_id: &[u8; 16]) -> i64 {
    stable_oid_bytes(kind, object_id.iter().copied())
}

fn stable_oid_bytes(kind: &str, name: impl Iterator<Item = u8>) -> i64 {
    let mut hash = 14_695_981_039_346_656_037_u64;
    for byte in kind.bytes().chain(*b":").chain(name) {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(1_099_511_628_211);
    }
    10_000 + i64::try_from(hash % 2_000_000_000).unwrap_or(0)
}

pub fn schema_oid(schema: &str) -> i64 {
    match schema {
        "pg_catalog" => 11,
        "public" => 2200,
        "information_schema" => 13_293,
        other => stable_oid("namespace", other),
    }
}

pub fn relation_oid(kind: &str, schema: &str, name: &str) -> i64 {
    stable_oid(kind, &format!("{schema}.{name}"))
}

pub fn current_user_oid() -> i64 {
    10
}

pub fn current_user_name() -> &'static str {
    "uqa"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numeric_identities_preserve_decimal_hashes_without_allocating() {
        for oid in [0, 1, 9, 10, 99, 100, 16_384, 1_000_000_000, u32::MAX] {
            let expected = stable_oid("domain_array", &oid.to_string());
            let allocations = allocation_counter::measure(|| {
                assert_eq!(stable_numeric_oid("domain_array", oid), expected);
            });
            assert_eq!(allocations.count_total, 0);
        }
    }
}
