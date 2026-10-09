//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `PostgreSQL` 18 polymorphic pseudo types shared by routine catalog output.

use super::{schema_oid, PgTypeCatalogMetadata, PgTypeRoutineOids};

pub(super) fn metadata() -> impl Iterator<Item = PgTypeCatalogMetadata<'static>> {
    [
        ("anyelement", 4, true, 2312, 2313, 0, 0, "i", "p"),
        ("anynonarray", 4, true, 2777, 2778, 0, 0, "i", "p"),
        ("anyenum", 4, true, 3504, 3505, 0, 0, "i", "p"),
        ("anyrange", -1, false, 3832, 3833, 0, 0, "d", "x"),
        ("anymultirange", -1, false, 4229, 4230, 0, 0, "d", "x"),
        (
            "anycompatiblemultirange",
            -1,
            false,
            4226,
            4227,
            0,
            0,
            "d",
            "x",
        ),
        ("anycompatible", 4, true, 5086, 5087, 0, 0, "i", "p"),
        (
            "anycompatiblearray",
            -1,
            false,
            5088,
            5089,
            5090,
            5091,
            "d",
            "x",
        ),
        ("anycompatiblenonarray", 4, true, 5092, 5093, 0, 0, "i", "p"),
        ("anycompatiblerange", -1, false, 5094, 5095, 0, 0, "d", "x"),
    ]
    .into_iter()
    .map(
        |(name, len, by_value, input, output, receive, send, align, storage)| {
            PgTypeCatalogMetadata {
                oid: uqa_sql::catalog::type_metadata::routine_type_oid(name),
                name: name.into(),
                namespace_oid: schema_oid("pg_catalog"),
                len,
                by_value,
                kind: "p",
                category: "P",
                preferred: false,
                relation_oid: 0,
                subscript: 0,
                element_oid: 0,
                array_oid: 0,
                routines: PgTypeRoutineOids {
                    input,
                    output,
                    receive,
                    send,
                    modifier_input: 0,
                    modifier_output: 0,
                    analyze: 0,
                },
                align,
                storage,
                base_oid: 0,
                type_modifier: -1,
                collation_oid: 0,
            }
        },
    )
}
