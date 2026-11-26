//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `PostgreSQL` expression pseudo-types project the same catalog attributes used in result descriptors.

use super::{schema_oid, PgTypeCatalogMetadata};
use uqa_sql::{ast::ExpressionPseudoType as P, catalog::type_metadata, ColumnType};

pub(super) fn metadata() -> impl Iterator<Item = PgTypeCatalogMetadata<'static>> {
    [
        P::Any,
        P::AnyElement,
        P::AnyNonArray,
        P::AnyEnum,
        P::AnyRange,
        P::AnyMultiRange,
        P::AnyCompatibleMultiRange,
        P::AnyCompatible,
        P::AnyCompatibleArray,
        P::AnyCompatibleNonArray,
        P::AnyCompatibleRange,
    ]
    .into_iter()
    .map(describe)
}

pub(super) fn describe(pseudo: P) -> PgTypeCatalogMetadata<'static> {
    let ty = ColumnType::Pseudo(pseudo);
    PgTypeCatalogMetadata {
        oid: type_metadata::pg_type_oid(&ty),
        name: pseudo.name().into(),
        namespace_oid: schema_oid("pg_catalog"),
        len: type_metadata::pg_type_len(&ty),
        by_value: type_metadata::pg_type_by_value(&ty),
        kind: "p",
        category: if pseudo == P::Unknown { "X" } else { "P" },
        preferred: false,
        relation_oid: 0,
        subscript: 0,
        element_oid: 0,
        array_oid: 0,
        routines: type_metadata::pg_type_routine_oids(&ty),
        align: type_metadata::pg_type_align(&ty),
        storage: type_metadata::pg_type_storage(&ty),
        base_oid: 0,
        type_modifier: -1,
        collation_oid: 0,
    }
}
