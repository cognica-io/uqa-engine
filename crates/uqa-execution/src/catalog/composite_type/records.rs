//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Independent composite type definitions and claims for the type, array and relation OIDs they own.

use std::collections::BTreeSet;
use uqa_sql::catalog::composite_type::StoredComposite;

use crate::catalog::type_records::{TypeRecord, TypeRecordFormat};

pub(super) const FORMAT: TypeRecordFormat = TypeRecordFormat {
    label: "composite",
    definition_context: "composite definition",
    oid_context: "composite OID",
    format_key: "sql_composites_json",
    format_field: "composite_catalog_format",
    version: 1,
    prefix: "uqa.sql.composite.v1:",
    oid_prefix: "uqa.sql.composite_oid.v1:",
};

impl TypeRecord for StoredComposite {
    fn claimed_oids(&self) -> BTreeSet<u32> {
        BTreeSet::from([self.relation_oid, self.array_oid, self.oid])
    }
}
