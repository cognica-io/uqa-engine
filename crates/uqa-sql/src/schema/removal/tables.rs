//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Detect ordinary-table schema dependencies and detach inbound foreign-key metadata.
use crate::{
    ast::{ColumnDef, ForeignKey, TableCheck, TableKeyConstraint},
    schema::dependencies::rewrites::{
        schema_expr_references_relation, stored_relation_reference_matches,
    },
};
use std::ops::Deref;
use uqa_core::RelationIdentity;
pub type TableColumnsRead<'a> = Box<dyn Deref<Target = Vec<ColumnDef>> + 'a>;
pub type TableChecksRead<'a> = Box<dyn Deref<Target = Vec<TableCheck>> + 'a>;
pub type TableForeignKeysRead<'a> = Box<dyn Deref<Target = Vec<ForeignKey>> + 'a>;
pub type TableKeysRead<'a> = Box<dyn Deref<Target = Vec<TableKeyConstraint>> + 'a>;
pub trait TableRemovalMetadata {
    fn columns(&self) -> TableColumnsRead<'_>;
    fn table_checks(&self) -> TableChecksRead<'_>;
    fn foreign_keys(&self) -> TableForeignKeysRead<'_>;
    fn key_constraints(&self) -> TableKeysRead<'_>;
}
pub fn table_schema_references_relation(
    table: &dyn TableRemovalMetadata,
    target: &RelationIdentity,
) -> bool {
    table.columns().iter().any(|column| {
        column
            .default
            .as_ref()
            .is_some_and(|expr| schema_expr_references_relation(expr, target))
            || column
                .check
                .as_ref()
                .is_some_and(|expr| schema_expr_references_relation(expr, target))
            || column.generated.as_ref().is_some_and(|generated| {
                schema_expr_references_relation(&generated.expression, target)
            })
    }) || table
        .table_checks()
        .iter()
        .any(|check| schema_expr_references_relation(&check.expr, target))
}
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
pub fn detach_inbound_foreign_keys(
    columns: &mut [ColumnDef],
    foreign_keys: &mut Vec<ForeignKey>,
    targets: &[RelationIdentity],
) -> bool {
    let previous_fk_len = foreign_keys.len();
    foreign_keys.retain(|foreign_key| {
        !targets
            .iter()
            .any(|target| foreign_key_targets(foreign_key, target))
    });
    let mut changed = previous_fk_len != foreign_keys.len();
    for column in columns {
        if column.references.as_ref().is_some_and(|reference| {
            targets
                .iter()
                .any(|target| stored_relation_reference_matches(&reference.table, target))
        }) {
            column.references = None;
            changed = true;
        }
    }
    changed
}

#[cfg(test)]
mod tests;
