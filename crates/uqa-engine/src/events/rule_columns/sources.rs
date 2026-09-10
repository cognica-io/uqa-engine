//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Durable table input shapes, independent of the columns a statement actually reads.

use super::{
    ColumnBindingContext, ColumnBindingMode, ColumnScope, FromClause, RelationIdentity,
    RuleColumnBinder, SQLError,
};

impl RuleColumnBinder<'_> {
    pub(super) fn bind_table_source(
        &mut self,
        source: &mut FromClause,
        context: &ColumnBindingContext,
    ) -> Result<ColumnScope, SQLError> {
        let FromClause::Table {
            name,
            qualifier,
            alias,
            column_aliases,
            bound_columns,
            ..
        } = source
        else {
            unreachable!("table column binding requires a table source");
        };
        self.bind_table_source_columns(name, bound_columns, context)?;
        let scope = self.table_scope(
            name,
            qualifier,
            alias.as_deref(),
            column_aliases,
            bound_columns.as_deref(),
            context,
        )?;
        self.remove_dropped_column_aliases(column_aliases, &scope);
        if let Some(columns) = bound_columns {
            self.remove_dropped_column_aliases(columns, &scope);
        }
        Ok(scope)
    }

    pub(super) fn bind_table_source_columns(
        &mut self,
        name: &str,
        bound_columns: &mut Option<Vec<String>>,
        context: &ColumnBindingContext,
    ) -> Result<(), SQLError> {
        if context.ctes.contains_key(&name.to_ascii_lowercase()) {
            return Ok(());
        }
        if let (Some(columns), ColumnBindingMode::Rename { relation, from, to }) =
            (bound_columns, self.mode)
        {
            let identity = RelationIdentity::from_legacy_name(name).map_err(SQLError::Internal)?;
            if &identity == relation {
                for column in columns {
                    if column == from {
                        *column = to.to_string();
                        self.alias_shape_changed = true;
                    }
                }
            }
        }
        Ok(())
    }
}
