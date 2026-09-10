//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Preserve domain DROP type names for catalog resolution and diagnostics.

use super::{extract_string, render_relation_component, Result, SQLError};

pub(super) fn compile_drop_domain_name(ty: &pg_query::protobuf::TypeName) -> Result<String> {
    let parts = ty
        .names
        .iter()
        .map(extract_string)
        .collect::<Result<Vec<_>>>()?;
    if parts.is_empty() {
        return Err(SQLError::Internal("DROP DOMAIN target has no name".into()));
    }
    let mut name = parts
        .iter()
        .map(|part| render_relation_component(part))
        .collect::<Vec<_>>()
        .join(".");
    for _ in &ty.array_bounds {
        name.push_str("[]");
    }
    Ok(name)
}
