//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Index catalog decoding and definition rendering.

use super::super::pg_catalog::CatalogIndexRelation;
use uqa_sql::{expr::quote_ident, SQLError};

pub(in crate::sql::catalog) fn index_columns(columns_json: &str) -> Result<Vec<String>, SQLError> {
    serde_json::from_str(columns_json)
        .map_err(|err| SQLError::Internal(format!("decode index column catalog: {err}")))
}

pub(in crate::sql::catalog) fn indexdef(
    catalog: &crate::engine_capabilities::CatalogReadView,
    resolution: &crate::engine_capabilities::RelationNameResolution,
    index: &CatalogIndexRelation,
    table: &str,
    pretty: bool,
) -> Result<String, SQLError> {
    let method = if index.index_type.is_empty() {
        "btree"
    } else {
        &index.index_type
    };
    let unique = if index.definition.unique {
        "UNIQUE "
    } else {
        ""
    };
    let columns = index
        .columns
        .iter()
        .enumerate()
        .map(|(position, column)| {
            let mut column = quote_ident(column);
            let order = index
                .definition
                .column_order
                .get(position)
                .copied()
                .unwrap_or_default();
            if order.descending {
                column.push_str(" DESC");
            }
            if order.nulls_first != order.descending {
                column.push_str(if order.nulls_first {
                    " NULLS FIRST"
                } else {
                    " NULLS LAST"
                });
            }
            column
        })
        .collect::<Vec<_>>()
        .join(", ");
    let mut sql = format!(
        "CREATE {unique}INDEX {} ON {table} USING {method} ({columns})",
        quote_ident(&index.relation.name)
    );
    if !index.definition.included_columns.is_empty() {
        sql.push_str(" INCLUDE (");
        sql.push_str(
            &index
                .definition
                .included_columns
                .iter()
                .map(|column| quote_ident(column))
                .collect::<Vec<_>>()
                .join(", "),
        );
        sql.push(')');
    }
    if index.definition.nulls_not_distinct {
        sql.push_str(" NULLS NOT DISTINCT");
    }
    if let Some(predicate) = index.definition.predicate.as_deref() {
        let predicate = super::super::view_definition::stored_expression_definition(
            catalog, resolution, predicate, pretty,
        )?;
        sql.push_str(" WHERE ");
        sql.push_str(&predicate);
    }
    Ok(sql)
}
