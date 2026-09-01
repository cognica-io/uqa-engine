//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Static `pg_type` and `pg_range` projection.

use uqa_core::Value;
use uqa_sql::ast::{ColumnType, RangeSubtype};
use uqa_sql::ResultRow;

use super::super::helpers::oids::{current_user_oid, schema_oid};
use super::super::helpers::rows::{bool_value, int_value, row, str_value};
use super::super::helpers::type_metadata::{
    pg_type_align, pg_type_array_oid, pg_type_by_value, pg_type_collation_oid, pg_type_element_oid,
    pg_type_len, pg_type_oid, pg_type_routine_oids, pg_type_storage, pg_type_subscript_handler,
    PgTypeRoutineOids,
};

#[expect(
    clippy::too_many_lines,
    reason = "preserves catalog column and OID order"
)]
pub(in crate::sql::catalog) fn build_pg_type() -> Vec<ResultRow> {
    let catalog_types = [
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
        (ColumnType::Regnamespace, "N", false, "b"),
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
    let mut types = catalog_types
        .iter()
        .cloned()
        .chain(
            catalog_types
                .iter()
                .filter(|&(ty, _, _, kind)| {
                    matches!(*kind, "b" | "r" | "m") && pg_type_array_oid(ty) != 0
                })
                .cloned()
                .map(|(ty, _, _, _)| (ColumnType::Array(Box::new(ty)), "A", false, "b")),
        )
        .map(|(ty, category, preferred, kind)| {
            pg_type_catalog_row(
                &ty,
                schema_oid("pg_catalog"),
                kind,
                category,
                preferred,
                0,
                -1,
            )
        })
        .collect::<Vec<_>>();
    for domain in super::super::schema::information_schema_domains() {
        let ColumnType::Domain { oid, base, .. } = &domain else {
            unreachable!("information schema type constructor returned a non-domain")
        };
        let (category, type_modifier) = match *oid {
            13_307 => ("N", -1),
            13_310 | 13_312 => ("S", -1),
            13_318 => ("D", 2),
            13_320 => ("S", 7),
            _ => unreachable!("unknown PostgreSQL 18 information schema domain {oid}"),
        };
        types.push(pg_type_catalog_row(
            &domain,
            schema_oid("information_schema"),
            "d",
            category,
            false,
            pg_type_oid(base),
            type_modifier,
        ));
        types.push(pg_type_catalog_row(
            &ColumnType::Array(Box::new(domain)),
            schema_oid("information_schema"),
            "b",
            "A",
            false,
            0,
            -1,
        ));
    }
    for domain in super::super::schema::ag_catalog_domains() {
        let ColumnType::Domain { name, base, .. } = &domain else {
            unreachable!("ag_catalog type constructor returned a non-domain")
        };
        let category = match name.as_str() {
            "label_id" => "N",
            "label_kind" => "Z",
            other => unreachable!("unknown ag_catalog domain {other}"),
        };
        types.push(pg_type_catalog_row(
            &domain,
            schema_oid(super::super::schema::AG_CATALOG_SCHEMA),
            "d",
            category,
            false,
            pg_type_oid(base),
            -1,
        ));
        types.push(pg_type_catalog_row(
            &ColumnType::Array(Box::new(domain)),
            schema_oid(super::super::schema::AG_CATALOG_SCHEMA),
            "b",
            "A",
            false,
            0,
            -1,
        ));
    }
    types.extend(super::super::ag_catalog::age_pg_type_rows());
    types.extend([
        special_pg_type_catalog_row(PgTypeCatalogMetadata {
            oid: 2249,
            name: "record".into(),
            namespace_oid: schema_oid("pg_catalog"),
            len: -1,
            by_value: false,
            kind: "p",
            category: "P",
            preferred: false,
            relation_oid: 0,
            subscript: "-",
            element_oid: 0,
            array_oid: 2287,
            routines: PgTypeRoutineOids {
                input: 2290,
                output: 2291,
                receive: 2402,
                send: 2403,
                modifier_input: 0,
                modifier_output: 0,
                analyze: 0,
            },
            align: "d",
            storage: "x",
            base_oid: 0,
            type_modifier: -1,
            collation_oid: 0,
        }),
        special_pg_type_catalog_row(PgTypeCatalogMetadata {
            oid: 2278,
            name: "void".into(),
            namespace_oid: schema_oid("pg_catalog"),
            len: 4,
            by_value: true,
            kind: "p",
            category: "P",
            preferred: false,
            relation_oid: 0,
            subscript: "-",
            element_oid: 0,
            array_oid: 0,
            routines: PgTypeRoutineOids {
                input: 2298,
                output: 2299,
                receive: 3120,
                send: 3121,
                modifier_input: 0,
                modifier_output: 0,
                analyze: 0,
            },
            align: "i",
            storage: "p",
            base_oid: 0,
            type_modifier: -1,
            collation_oid: 0,
        }),
        special_pg_type_catalog_row(PgTypeCatalogMetadata {
            oid: 2287,
            name: "_record".into(),
            namespace_oid: schema_oid("pg_catalog"),
            len: -1,
            by_value: false,
            kind: "p",
            category: "P",
            preferred: false,
            relation_oid: 0,
            subscript: "array_subscript_handler",
            element_oid: 2249,
            array_oid: 0,
            routines: PgTypeRoutineOids {
                input: 750,
                output: 751,
                receive: 2400,
                send: 2401,
                modifier_input: 0,
                modifier_output: 0,
                analyze: 3816,
            },
            align: "d",
            storage: "x",
            base_oid: 0,
            type_modifier: -1,
            collation_oid: 0,
        }),
        special_pg_type_catalog_row(PgTypeCatalogMetadata {
            oid: 13_314,
            name: "_information_schema_catalog_name".into(),
            namespace_oid: schema_oid("information_schema"),
            len: -1,
            by_value: false,
            kind: "b",
            category: "A",
            preferred: false,
            relation_oid: 0,
            subscript: "array_subscript_handler",
            element_oid: 13_315,
            array_oid: 0,
            routines: PgTypeRoutineOids {
                input: 750,
                output: 751,
                receive: 2400,
                send: 2401,
                modifier_input: 0,
                modifier_output: 0,
                analyze: 3816,
            },
            align: "d",
            storage: "x",
            base_oid: 0,
            type_modifier: -1,
            collation_oid: 0,
        }),
        special_pg_type_catalog_row(PgTypeCatalogMetadata {
            oid: 13_315,
            name: "information_schema_catalog_name".into(),
            namespace_oid: schema_oid("information_schema"),
            len: -1,
            by_value: false,
            kind: "c",
            category: "C",
            preferred: false,
            relation_oid: 13_313,
            subscript: "-",
            element_oid: 0,
            array_oid: 13_314,
            routines: PgTypeRoutineOids {
                input: 2290,
                output: 2291,
                receive: 2402,
                send: 2403,
                modifier_input: 0,
                modifier_output: 0,
                analyze: 0,
            },
            align: "d",
            storage: "x",
            base_oid: 0,
            type_modifier: -1,
            collation_oid: 0,
        }),
    ]);
    types.sort_by_key(|entry| match entry.get("oid") {
        Some(Value::Int(oid)) => *oid,
        _ => i64::MAX,
    });
    types
}

pub(in crate::sql::catalog) fn build_pg_range() -> Vec<ResultRow> {
    [
        (RangeSubtype::Integer, 1_978, 3_914, 3_922),
        (RangeSubtype::Numeric, 3_125, 0, 3_924),
        (RangeSubtype::Timestamp, 3_128, 0, 3_929),
        (RangeSubtype::TimestampTz, 3_127, 0, 3_930),
        (RangeSubtype::Date, 3_122, 3_915, 3_925),
        (RangeSubtype::BigInteger, 3_124, 3_928, 3_923),
    ]
    .into_iter()
    .map(|(subtype, subtype_opclass, canonical, subtype_diff)| {
        row([
            (
                "rngtypid",
                int_value(pg_type_oid(&ColumnType::Range(subtype))),
            ),
            ("rngsubtype", int_value(pg_type_oid(&subtype.scalar_type()))),
            (
                "rngmultitypid",
                int_value(pg_type_oid(&ColumnType::Multirange(subtype))),
            ),
            ("rngcollation", int_value(0)),
            ("rngsubopc", int_value(subtype_opclass)),
            ("rngcanonical", int_value(canonical)),
            ("rngsubdiff", int_value(subtype_diff)),
        ])
    })
    .collect()
}

struct PgTypeCatalogMetadata<'a> {
    oid: i64,
    name: String,
    namespace_oid: i64,
    len: i64,
    by_value: bool,
    kind: &'a str,
    category: &'a str,
    preferred: bool,
    relation_oid: i64,
    subscript: &'a str,
    element_oid: i64,
    array_oid: i64,
    routines: PgTypeRoutineOids,
    align: &'a str,
    storage: &'a str,
    base_oid: i64,
    type_modifier: i64,
    collation_oid: i64,
}

fn pg_type_catalog_row(
    ty: &ColumnType,
    namespace_oid: i64,
    kind: &str,
    category: &str,
    preferred: bool,
    base_oid: i64,
    type_modifier: i64,
) -> ResultRow {
    special_pg_type_catalog_row(PgTypeCatalogMetadata {
        oid: pg_type_oid(ty),
        name: super::super::helpers::information_schema_types::info_udt_name(ty),
        namespace_oid,
        len: pg_type_len(ty),
        by_value: pg_type_by_value(ty),
        kind,
        category,
        preferred,
        relation_oid: 0,
        subscript: pg_type_subscript_handler(ty),
        element_oid: pg_type_element_oid(ty),
        array_oid: pg_type_array_oid(ty),
        routines: pg_type_routine_oids(ty),
        align: pg_type_align(ty),
        storage: pg_type_storage(ty),
        base_oid,
        type_modifier,
        collation_oid: pg_type_collation_oid(ty),
    })
}

fn special_pg_type_catalog_row(metadata: PgTypeCatalogMetadata<'_>) -> ResultRow {
    row([
        ("oid", int_value(metadata.oid)),
        ("typname", str_value(metadata.name)),
        ("typnamespace", int_value(metadata.namespace_oid)),
        ("typowner", int_value(current_user_oid())),
        ("typlen", int_value(metadata.len)),
        ("typbyval", bool_value(metadata.by_value)),
        ("typtype", str_value(metadata.kind)),
        ("typcategory", str_value(metadata.category)),
        ("typispreferred", bool_value(metadata.preferred)),
        ("typisdefined", bool_value(true)),
        ("typdelim", str_value(",")),
        ("typrelid", int_value(metadata.relation_oid)),
        ("typsubscript", str_value(metadata.subscript)),
        ("typelem", int_value(metadata.element_oid)),
        ("typarray", int_value(metadata.array_oid)),
        ("typinput", int_value(metadata.routines.input)),
        ("typoutput", int_value(metadata.routines.output)),
        ("typreceive", int_value(metadata.routines.receive)),
        ("typsend", int_value(metadata.routines.send)),
        ("typmodin", int_value(metadata.routines.modifier_input)),
        ("typmodout", int_value(metadata.routines.modifier_output)),
        ("typanalyze", int_value(metadata.routines.analyze)),
        ("typalign", str_value(metadata.align)),
        ("typstorage", str_value(metadata.storage)),
        ("typnotnull", bool_value(false)),
        ("typbasetype", int_value(metadata.base_oid)),
        ("typtypmod", int_value(metadata.type_modifier)),
        ("typndims", int_value(0)),
        ("typcollation", int_value(metadata.collation_oid)),
        ("typdefaultbin", Value::Null),
        ("typdefault", Value::Null),
        ("typacl", Value::Null),
    ])
}
