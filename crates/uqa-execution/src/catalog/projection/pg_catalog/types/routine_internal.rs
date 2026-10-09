//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Catalog-only argument types used by builtin input, output and support routines.

use super::{schema_oid, PgTypeCatalogMetadata, PgTypeRoutineOids};

pub(super) fn metadata() -> impl Iterator<Item = PgTypeCatalogMetadata<'static>> {
    [
        (
            3115,
            "fdw_handler",
            4,
            true,
            "p",
            "P",
            0,
            0,
            3116,
            3117,
            0,
            0,
            "i",
            "p",
        ),
        (
            2275, "cstring", -2, false, "p", "P", 0, 1263, 2292, 2293, 2500, 2501, "c", "p",
        ),
        (
            2281, "internal", 8, true, "p", "P", 0, 0, 2304, 2305, 0, 0, "d", "p",
        ),
        (
            1263, "_cstring", -1, false, "b", "A", 2275, 0, 750, 751, 2400, 2401, "i", "x",
        ),
    ]
    .into_iter()
    .map(
        |(
            oid,
            name,
            len,
            by_value,
            kind,
            category,
            element_oid,
            array_oid,
            input,
            output,
            receive,
            send,
            align,
            storage,
        )| {
            PgTypeCatalogMetadata {
                oid,
                name: name.into(),
                namespace_oid: schema_oid("pg_catalog"),
                len,
                by_value,
                kind,
                category,
                preferred: false,
                relation_oid: 0,
                subscript: if element_oid == 0 { 0 } else { 6179 },
                element_oid,
                array_oid,
                routines: PgTypeRoutineOids {
                    input,
                    output,
                    receive,
                    send,
                    modifier_input: 0,
                    modifier_output: 0,
                    analyze: if element_oid == 0 { 0 } else { 3816 },
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
