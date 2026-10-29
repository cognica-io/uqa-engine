//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Undefined type rows keep their identity visible without advertising a value layout or array.

use super::{
    bool_value, int_value, namespace_oid, special_pg_type_catalog_row, type_acl_value,
    CatalogReadView, PgTypeCatalogMetadata, PgTypeRoutineOids,
};
use uqa_sql::{ResultRow, SQLError};

pub(super) fn rows(catalog: &CatalogReadView) -> Result<Vec<ResultRow>, SQLError> {
    catalog
        .shells()
        .map(|definition| {
            let mut row = special_pg_type_catalog_row(PgTypeCatalogMetadata {
                oid: i64::from(definition.oid),
                name: definition.identity.name.as_str().into(),
                namespace_oid: namespace_oid(catalog, &definition.identity.schema),
                len: 4,
                by_value: true,
                kind: "p",
                category: "P",
                preferred: false,
                relation_oid: 0,
                subscript: 0,
                element_oid: 0,
                array_oid: 0,
                routines: PgTypeRoutineOids {
                    input: 2398,
                    output: 2399,
                    receive: 0,
                    send: 0,
                    modifier_input: 0,
                    modifier_output: 0,
                    analyze: 0,
                },
                align: "i",
                storage: "p",
                base_oid: 0,
                type_modifier: -1,
                collation_oid: 0,
            });
            row.insert("typisdefined".into(), bool_value(false));
            row.insert("typowner".into(), int_value(definition.owner.oid));
            row.insert(
                "typacl".into(),
                type_acl_value(catalog, definition.usage_acl.as_deref())?,
            );
            Ok(row)
        })
        .collect()
}
