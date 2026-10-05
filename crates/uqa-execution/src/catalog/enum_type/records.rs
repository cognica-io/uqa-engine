//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Independent enum definitions and claims for every public OID they own.

use std::collections::BTreeSet;
use uqa_sql::catalog::enum_type::StoredEnum;

use crate::catalog::type_records::{TypeRecord, TypeRecordFormat};

pub(super) const FORMAT: TypeRecordFormat = TypeRecordFormat {
    label: "enum",
    definition_context: "enum definition",
    oid_context: "enum OID",
    format_key: "sql_enums_json",
    format_field: "enum_catalog_format",
    version: 1,
    prefix: "uqa.sql.enum.v1:",
    oid_prefix: "uqa.sql.enum_oid.v1:",
};

impl TypeRecord for StoredEnum {
    fn claimed_oids(&self) -> BTreeSet<u32> {
        [self.oid, self.array_oid]
            .into_iter()
            .chain(self.label_oids())
            .collect()
    }
}
