//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! The table a foreign key references, and the table a table DDL statement names.
use crate::schema::dependencies::rewrites::stored_relation_reference_matches;
use uqa_core::RelationIdentity;
pub fn foreign_key_targets(
    foreign_key: &crate::ast::ForeignKey,
    target: &RelationIdentity,
) -> bool {
    stored_relation_reference_matches(&foreign_key.ref_table, target)
}
pub fn resolved_table_ddl_target(
    resolved: Option<(String, &str)>,
    action: &str,
) -> Result<Option<String>, String> {
    match resolved {
        Some((canonical, "table")) => Ok(Some(canonical)),
        Some((canonical, kind)) => Err(format!(
            "{action}: relation `{canonical}` is a {kind}, not a table"
        )),
        None => Ok(None),
    }
}

#[cfg(test)]
mod tests;
