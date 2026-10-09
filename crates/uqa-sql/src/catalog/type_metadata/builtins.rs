//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Resolve builtin physical identities through the catalog's existing OID definitions.

use super::{pg_type_array_oid, pg_type_oid, ColumnType, RangeSubtype};

const TYPES: &[ColumnType] = &[
    ColumnType::Boolean,
    ColumnType::Bytea,
    ColumnType::InternalChar,
    ColumnType::Name,
    ColumnType::BigInteger,
    ColumnType::SmallInteger,
    ColumnType::Int2Vector,
    ColumnType::Integer,
    ColumnType::Regproc,
    ColumnType::Text,
    ColumnType::Oid,
    ColumnType::Xid,
    ColumnType::OidVector,
    ColumnType::Json,
    ColumnType::PgNodeTree,
    ColumnType::Real,
    ColumnType::DoublePrecision,
    ColumnType::AclItem,
    ColumnType::Bpchar,
    ColumnType::Varchar(None),
    ColumnType::Date,
    ColumnType::Time,
    ColumnType::TimeTz,
    ColumnType::Timestamp,
    ColumnType::TimestampTz,
    ColumnType::Interval,
    ColumnType::Numeric {
        precision: None,
        scale: None,
    },
    ColumnType::RefCursor,
    ColumnType::Regprocedure,
    ColumnType::Regclass,
    ColumnType::Regtype,
    ColumnType::Record,
    ColumnType::Uuid,
    ColumnType::JsonB,
    ColumnType::Regnamespace,
    ColumnType::Regrole,
    ColumnType::Regcollation,
    ColumnType::Range(RangeSubtype::Integer),
    ColumnType::Range(RangeSubtype::Numeric),
    ColumnType::Range(RangeSubtype::Timestamp),
    ColumnType::Range(RangeSubtype::TimestampTz),
    ColumnType::Range(RangeSubtype::Date),
    ColumnType::Range(RangeSubtype::BigInteger),
    ColumnType::Multirange(RangeSubtype::Integer),
    ColumnType::Multirange(RangeSubtype::Numeric),
    ColumnType::Multirange(RangeSubtype::Timestamp),
    ColumnType::Multirange(RangeSubtype::TimestampTz),
    ColumnType::Multirange(RangeSubtype::Date),
    ColumnType::Multirange(RangeSubtype::BigInteger),
];

/// The builtin scalar type with this physical OID, without a declaration's typmod.
pub fn builtin_scalar_type(oid: u32) -> Option<&'static ColumnType> {
    TYPES.iter().find(|ty| pg_type_oid(ty) == i64::from(oid))
}

/// The element declaration of a builtin SQL array, identified by the array's OID.
pub fn builtin_array_element(oid: u32) -> Option<&'static ColumnType> {
    (oid != 0)
        .then(|| {
            TYPES
                .iter()
                .find(|ty| pg_type_array_oid(ty) == i64::from(oid))
        })
        .flatten()
}
