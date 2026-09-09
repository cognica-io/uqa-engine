//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Result type identities shared with the `PostgreSQL` catalog.

use uqa_sql::ast::ColumnType;

use super::helpers::type_metadata::{pg_type_len, pg_type_modifier, pg_type_oid};

/// The type fields in a `PostgreSQL` result-column descriptor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SQLTypeMetadata {
    pub type_oid: u32,
    pub type_size: i16,
    pub type_modifier: i32,
}

/// Resolve result metadata without inspecting runtime values. `PostgreSQL` flattens a domain to its base type at the client boundary, while arrays retain their element type identity.
pub fn postgres_result_type(ty: &ColumnType) -> SQLTypeMetadata {
    if let ColumnType::Domain { base, .. } = ty {
        return postgres_result_type(base);
    }
    SQLTypeMetadata {
        type_oid: pg_type_oid(ty) as u32,
        type_size: pg_type_len(ty) as i16,
        type_modifier: pg_type_modifier(ty) as i32,
    }
}
