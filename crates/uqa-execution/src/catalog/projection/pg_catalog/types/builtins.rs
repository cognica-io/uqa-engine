//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Immutable builtin type identities shared with the complete catalog projection.

use super::{pg_type_array_oid, pg_type_oid, polymorphic, routine_internal, special};
use std::{collections::BTreeSet, sync::OnceLock};
use uqa_sql::ast::{ColumnType, RangeSubtype};

pub(super) const CATALOG_TYPES: &[(ColumnType, &str, bool, &str)] = &[
    (ColumnType::Boolean, "B", true, "b"),
    (ColumnType::Bytea, "U", false, "b"),
    (ColumnType::InternalChar, "Z", false, "b"),
    (ColumnType::Name, "S", false, "b"),
    (ColumnType::BigInteger, "N", false, "b"),
    (ColumnType::Int2Vector, "A", false, "b"),
    (ColumnType::SmallInteger, "N", false, "b"),
    (ColumnType::Integer, "N", false, "b"),
    (ColumnType::Regproc, "N", false, "b"),
    (ColumnType::Regprocedure, "N", false, "b"),
    (ColumnType::Regclass, "N", false, "b"),
    (ColumnType::Text, "S", true, "b"),
    (ColumnType::RefCursor, "U", false, "b"),
    (ColumnType::Oid, "N", true, "b"),
    (ColumnType::Xid, "U", false, "b"),
    (ColumnType::OidVector, "A", false, "b"),
    (ColumnType::Json, "U", false, "b"),
    (ColumnType::PgNodeTree, "Z", false, "b"),
    (ColumnType::Real, "N", false, "b"),
    (ColumnType::DoublePrecision, "N", true, "b"),
    (ColumnType::AclItem, "U", false, "b"),
    (ColumnType::Bpchar, "S", false, "b"),
    (ColumnType::Varchar(None), "S", false, "b"),
    (ColumnType::Date, "D", false, "b"),
    (ColumnType::Time, "D", false, "b"),
    (ColumnType::Timestamp, "D", false, "b"),
    (ColumnType::TimestampTz, "D", true, "b"),
    (ColumnType::Interval, "T", true, "b"),
    (ColumnType::TimeTz, "D", false, "b"),
    (
        ColumnType::Numeric {
            precision: None,
            scale: None,
        },
        "N",
        false,
        "b",
    ),
    (ColumnType::Regtype, "N", false, "b"),
    (ColumnType::Regcollation, "N", false, "b"),
    (ColumnType::Regnamespace, "N", false, "b"),
    (ColumnType::Regrole, "N", false, "b"),
    (ColumnType::AnyArray, "P", false, "p"),
    (ColumnType::Uuid, "U", false, "b"),
    (ColumnType::JsonB, "U", false, "b"),
    (ColumnType::Range(RangeSubtype::Integer), "R", false, "r"),
    (ColumnType::Range(RangeSubtype::Numeric), "R", false, "r"),
    (ColumnType::Range(RangeSubtype::Timestamp), "R", false, "r"),
    (
        ColumnType::Range(RangeSubtype::TimestampTz),
        "R",
        false,
        "r",
    ),
    (ColumnType::Range(RangeSubtype::Date), "R", false, "r"),
    (ColumnType::Range(RangeSubtype::BigInteger), "R", false, "r"),
    (
        ColumnType::Multirange(RangeSubtype::Integer),
        "R",
        false,
        "m",
    ),
    (
        ColumnType::Multirange(RangeSubtype::Numeric),
        "R",
        false,
        "m",
    ),
    (
        ColumnType::Multirange(RangeSubtype::Timestamp),
        "R",
        false,
        "m",
    ),
    (
        ColumnType::Multirange(RangeSubtype::TimestampTz),
        "R",
        false,
        "m",
    ),
    (ColumnType::Multirange(RangeSubtype::Date), "R", false, "m"),
    (
        ColumnType::Multirange(RangeSubtype::BigInteger),
        "R",
        false,
        "m",
    ),
    (ColumnType::Vector(0), "U", false, "b"),
    (ColumnType::Tensor(0), "U", false, "b"),
];

pub(in crate::catalog::projection) fn builtin_type_oid_in_use(oid: i64) -> bool {
    static OIDS: OnceLock<BTreeSet<i64>> = OnceLock::new();
    OIDS.get_or_init(|| {
        let mut oids = CATALOG_TYPES
            .iter()
            .flat_map(|(ty, _, _, kind)| {
                let array = matches!(*kind, "b" | "r" | "m").then(|| pg_type_array_oid(ty));
                std::iter::once(pg_type_oid(ty)).chain(array.filter(|oid| *oid != 0))
            })
            .collect::<BTreeSet<_>>();
        oids.extend(polymorphic::metadata().map(|ty| ty.oid));
        oids.extend(routine_internal::metadata().map(|ty| ty.oid));
        oids.extend(special::metadata().map(|ty| ty.oid));
        let domains = super::super::super::schema::information_schema_domains()
            .into_iter()
            .chain(super::super::super::schema::ag_catalog_domains());
        oids.extend(domains.flat_map(|ty| [pg_type_oid(&ty), pg_type_array_oid(&ty)]));
        oids.extend(super::super::super::ag_catalog::age_type_oids());
        oids.extend(super::super::languages::type_oids());
        oids.extend(
            uqa_sql::catalog::foreign_relations::FOREIGN_CATALOGS
                .iter()
                .flat_map(|definition| {
                    [
                        i64::from(definition.row_type),
                        i64::from(definition.array_type),
                    ]
                }),
        );
        oids
    })
    .contains(&oid)
}
