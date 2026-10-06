//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Catalog-only argument type of builtin planner support functions.

use super::{
    schema_oid, special_pg_type_catalog_row, PgTypeCatalogMetadata, PgTypeRoutineOids, ResultRow,
};

pub(super) fn rows() -> impl Iterator<Item = ResultRow> {
    std::iter::once(special_pg_type_catalog_row(PgTypeCatalogMetadata {
        oid: 2281,
        name: "internal".into(),
        namespace_oid: schema_oid("pg_catalog"),
        len: 8,
        by_value: true,
        kind: "p",
        category: "P",
        preferred: false,
        relation_oid: 0,
        subscript: 0,
        element_oid: 0,
        array_oid: 0,
        routines: PgTypeRoutineOids {
            input: 2304,
            output: 2305,
            receive: 0,
            send: 0,
            modifier_input: 0,
            modifier_output: 0,
            analyze: 0,
        },
        align: "d",
        storage: "p",
        base_oid: 0,
        type_modifier: -1,
        collation_oid: 0,
    }))
}
