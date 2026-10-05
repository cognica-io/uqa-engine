//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Stored values follow a composite type's attribute changes: every table column and materialized view column whose declared type holds values of the type is rewritten.

use std::collections::BTreeMap;

use crate::mutation::constraints::context::MutationRead;
use crate::schema::columns::ColumnRewritePublication;
use crate::schema::view_dependencies::ViewDependencyContext;
use uqa_sql::ast::ColumnDef;
use uqa_sql::expr::composites::{
    apply_attribute_change, type_contains_composite, AttributeChange, CompositeTypeCatalog,
};
use uqa_sql::SQLError;

/// The tables of the catalog with their declared columns.
pub trait CompositeValueTables {
    fn composite_value_tables(&self) -> Result<Vec<(String, Vec<ColumnDef>)>, SQLError>;
}

pub struct CompositeValueContext<'a> {
    pub tables: &'a dyn CompositeValueTables,
    pub reads: &'a dyn MutationRead,
    pub writes: &'a dyn ColumnRewritePublication,
    pub views: ViewDependencyContext<'a>,
    /// The attributes of every composite type before the change.
    pub types: &'a dyn CompositeTypeCatalog,
}

/// Rewrite the stored values of composite type `target` for `change`, in tables and materialized views.
pub fn rewrite_composite_values(
    context: &CompositeValueContext<'_>,
    target: u32,
    change: &AttributeChange,
) -> Result<(), SQLError> {
    for (table, columns) in context.tables.composite_value_tables()? {
        let mut affected = Vec::new();
        for column in columns {
            if type_contains_composite(&column.ty, target, context.types)? {
                affected.push(column);
            }
        }
        if affected.is_empty() {
            continue;
        }
        for doc_id in context.reads.live_table_doc_ids(&table)? {
            let Some(document) = context.reads.get_document(&table, doc_id)? else {
                continue;
            };
            let mut updates = BTreeMap::new();
            for column in &affected {
                let Some(value) = document.get(&column.name).cloned() else {
                    continue;
                };
                updates.insert(
                    column.name.clone(),
                    apply_attribute_change(value, &column.ty, target, change, context.types)?,
                );
            }
            if !updates.is_empty() {
                context
                    .writes
                    .update_fields(&table, doc_id, updates, BTreeMap::new())?;
            }
        }
    }
    crate::schema::view_dependencies::rewrite_materialized_composite_values(
        &context.views,
        target,
        change,
        context.types,
    )
}
