//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Resolve builtin physical identities through the catalog's existing OID definitions.

use super::{pg_type_array_oid, pg_type_oid, ColumnType, RangeSubtype};

const TYPES: &[(ColumnType, &str)] = &[
    (ColumnType::Boolean, "bool"),
    (ColumnType::Bytea, "bytea"),
    (ColumnType::InternalChar, "char"),
    (ColumnType::Name, "name"),
    (ColumnType::BigInteger, "int8"),
    (ColumnType::SmallInteger, "int2"),
    (ColumnType::Int2Vector, "int2vector"),
    (ColumnType::Integer, "int4"),
    (ColumnType::Regproc, "regproc"),
    (ColumnType::Text, "text"),
    (ColumnType::Oid, "oid"),
    (ColumnType::Xid, "xid"),
    (ColumnType::OidVector, "oidvector"),
    (ColumnType::Json, "json"),
    (ColumnType::Xml, "xml"),
    (ColumnType::PgNodeTree, "pg_node_tree"),
    (ColumnType::Real, "float4"),
    (ColumnType::DoublePrecision, "float8"),
    (ColumnType::AclItem, "aclitem"),
    (ColumnType::Bpchar, "bpchar"),
    (ColumnType::Varchar(None), "varchar"),
    (ColumnType::Bit(None), "bit"),
    (ColumnType::VarBit(None), "varbit"),
    (ColumnType::Date, "date"),
    (ColumnType::Time, "time"),
    (ColumnType::TimeTz, "timetz"),
    (ColumnType::Timestamp, "timestamp"),
    (ColumnType::TimestampTz, "timestamptz"),
    (ColumnType::Interval, "interval"),
    (
        ColumnType::Numeric {
            precision: None,
            scale: None,
        },
        "numeric",
    ),
    (ColumnType::RefCursor, "refcursor"),
    (ColumnType::Regprocedure, "regprocedure"),
    (ColumnType::Regclass, "regclass"),
    (ColumnType::Regtype, "regtype"),
    (ColumnType::Record, "record"),
    (ColumnType::Void, "void"),
    (ColumnType::AnyArray, "anyarray"),
    (ColumnType::Uuid, "uuid"),
    (ColumnType::JsonB, "jsonb"),
    (ColumnType::Regnamespace, "regnamespace"),
    (ColumnType::Regrole, "regrole"),
    (ColumnType::Regcollation, "regcollation"),
    (ColumnType::Range(RangeSubtype::Integer), "int4range"),
    (ColumnType::Range(RangeSubtype::Numeric), "numrange"),
    (ColumnType::Range(RangeSubtype::Timestamp), "tsrange"),
    (ColumnType::Range(RangeSubtype::TimestampTz), "tstzrange"),
    (ColumnType::Range(RangeSubtype::Date), "daterange"),
    (ColumnType::Range(RangeSubtype::BigInteger), "int8range"),
    (
        ColumnType::Multirange(RangeSubtype::Integer),
        "int4multirange",
    ),
    (
        ColumnType::Multirange(RangeSubtype::Numeric),
        "nummultirange",
    ),
    (
        ColumnType::Multirange(RangeSubtype::Timestamp),
        "tsmultirange",
    ),
    (
        ColumnType::Multirange(RangeSubtype::TimestampTz),
        "tstzmultirange",
    ),
    (ColumnType::Multirange(RangeSubtype::Date), "datemultirange"),
    (
        ColumnType::Multirange(RangeSubtype::BigInteger),
        "int8multirange",
    ),
    (ColumnType::Vector(0), "vector"),
    (ColumnType::Tensor(0), "tensor"),
];

/// The builtin scalar type with this physical OID, without a declaration's typmod.
pub fn builtin_scalar_type(oid: u32) -> Option<&'static ColumnType> {
    TYPES
        .iter()
        .find(|(ty, _)| pg_type_oid(ty) == i64::from(oid))
        .map(|(ty, _)| ty)
}

/// Resolve an actual builtin catalog identifier, rather than a SQL keyword alias.
pub fn builtin_scalar_type_by_name(name: &str) -> Option<&'static ColumnType> {
    TYPES
        .iter()
        .find(|(_, candidate)| *candidate == name)
        .map(|(ty, _)| ty)
}

/// The catalog's local name for an implemented builtin type, independent of its modifiers.
pub fn builtin_type_name(ty: &ColumnType) -> Option<&'static str> {
    if matches!(
        ty,
        ColumnType::Named(_)
            | ColumnType::Array(_)
            | ColumnType::Domain { .. }
            | ColumnType::Enum(_)
            | ColumnType::Composite(_)
            | ColumnType::DeclaredRange(_)
            | ColumnType::DeclaredMultirange(_)
    ) {
        return None;
    }
    TYPES
        .iter()
        .find(|(candidate, _)| pg_type_oid(candidate) == pg_type_oid(ty))
        .map(|(_, name)| *name)
}

/// The local `pg_type.typname`, retaining user names and the shared array identity.
pub fn pg_type_name(ty: &ColumnType) -> String {
    match ty {
        ColumnType::Named(name) => {
            unreachable!("unresolved declaration type {name} reached catalog projection")
        }
        ColumnType::Domain { name, .. } => name.clone(),
        ColumnType::Enum(reference) => reference.name.clone(),
        ColumnType::Composite(reference) => reference.name.clone(),
        ColumnType::DeclaredRange(reference) | ColumnType::DeclaredMultirange(reference) => {
            reference.name.clone()
        }
        ColumnType::Array(element) => match element.as_ref() {
            ColumnType::Array(_) => pg_type_name(element),
            element => format!("_{}", pg_type_name(element)),
        },
        _ => builtin_type_name(ty)
            .expect("implemented builtin type has a catalog name")
            .into(),
    }
}

/// The element declaration of a builtin SQL array, identified by the array's OID.
pub fn builtin_array_element(oid: u32) -> Option<&'static ColumnType> {
    (oid != 0)
        .then(|| {
            TYPES
                .iter()
                .find(|(ty, _)| pg_type_array_oid(ty) == i64::from(oid))
                .map(|(ty, _)| ty)
        })
        .flatten()
}
